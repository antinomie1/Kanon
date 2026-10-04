//! Worker responsibilities.

use super::*;

impl PipelineEngine {
    /// Runs the asynchronous worker loop, draining events from the ingest receiver.
    ///
    /// For every ingested event, the worker runs the pipeline and routes any outbound
    /// replies to the registered outbound message sender channel.
    ///
    /// # Shutdown
    /// When [`PipelineEngine::drain`] starts, the ingest queue is closed (producers get an
    /// explicit `Closed` error), every event still queued is written to the dead-letter log
    /// without being started, and all active events share [`SHUTDOWN_EVENT_GRACE`] to finish
    /// before it is recorded as well. Nothing the node acknowledged disappears silently.
    ///
    /// # Scheduling
    /// Each chat runs in arrival order, including commands that change its current session.
    /// Distinct chats have bounded concurrency. `/stop` bypasses waiting chat lanes so it can
    /// interrupt the turn it addresses. Read-ahead is bounded by the ingest queue capacity;
    /// excess accepted events go to dead letter so a full queue cannot trap a later `/stop`.
    pub async fn run_worker_loop(&self, mut event_receiver: mpsc::Receiver<IngestEventRequest>) {
        let mut phase = self.shutdown.subscribe();
        let draining = |p| p != ShutdownPhase::Running;
        let mut waiting: VecDeque<(String, IngestEventRequest)> = VecDeque::new();
        let mut active = HashMap::<u64, (String, Option<PipelineEventRequest>)>::new();
        let mut next_task = 0_u64;
        let mut running: FuturesUnordered<BoxFuture<'_, u64>> = FuturesUnordered::new();
        let read_ahead = event_receiver.max_capacity();
        let mut queue_open = true;

        loop {
            if draining(*phase.borrow()) {
                break;
            }
            // A lane key excludes the generation: /new and /switch must finish before the next
            // event resolves the live session. Removing only eligible entries preserves FIFO
            // within a lane while allowing a different chat past a blocked one.
            // A simulation owner occupies its lane while listening. Admit at most one
            // additional ingress task for that lane, so post-filter deposits stay ordered.
            // Only the first waiting item of a lane is considered: slash commands are barriers
            // and cannot be overtaken by later chatter. Reserve ingress capacity for listeners.
            while running.len() < MAX_CONCURRENT_CHATS * 2 {
                let mut seen = std::collections::HashSet::new();
                let index = waiting.iter().position(|(key, req)| {
                    if !seen.insert(key) {
                        return false;
                    }
                    let count = active.values().filter(|(lane, _)| lane == key).count();
                    if count == 0 {
                        return running.len() < MAX_CONCURRENT_CHATS;
                    }
                    count == 1
                        && self.simulation.active(key)
                        && req.event.as_ref().is_some_and(|event| {
                            CommandRouter::parse_command(strip_leading_mentions(&message_text(
                                event,
                            )))
                            .is_none()
                                && NoticeKind::from_metadata(event.metadata.as_ref()).is_none()
                        })
                });
                let Some(index) = index else {
                    break;
                };
                let (key, req) = waiting.remove(index).expect("eligible queued chat");
                let id = next_task;
                next_task = next_task.wrapping_add(1);
                active.insert(id, (key, req.event.clone()));
                running.push(Box::pin(async move {
                    self.handle_ingested(req).await;
                    id
                }));
            }
            if !queue_open && waiting.is_empty() && running.is_empty() {
                break;
            }
            tokio::select! {
                biased;
                _ = wait_for_phase(&mut phase, draining) => break,
                Some(key) = running.next(), if !running.is_empty() => { active.remove(&key); }
                () = self.simulation.changed.notified() => {},
                req = event_receiver.recv(), if queue_open => {
                    match req {
                        Some(req) if is_stop_request(&req) => self.handle_ingested(req).await,
                        Some(req) if waiting.len() >= read_ahead => {
                            // Continue inspecting ingress for /stop even when chat lanes are full.
                            // Accepted overload is recorded explicitly instead of growing an
                            // unbounded staging queue or trapping stop behind a hung model.
                            if let Some(event) = req.event {
                                self.dead_letter_event(&event, "inbound chat waiting queue is full").await;
                            }
                        }
                        Some(req) => {
                            let key = match req.event.as_ref() {
                                Some(event) => match self.resolve_chat(event).await {
                                    Ok(chat) => format!("{}:{}", chat.instance.id, chat.conversation),
                                    Err(_) => conversation_key(event, false),
                                },
                                None => String::new(),
                            };
                            waiting.push_back((key, req));
                        }
                        None => queue_open = false,
                    }
                }
            }
        }

        // All running lanes share one deadline; N hung providers never multiply shutdown grace.
        let deadline = tokio::time::Instant::now() + SHUTDOWN_EVENT_GRACE;
        self.spill_queued_events(&mut waiting, &mut event_receiver)
            .await;
        while !running.is_empty() {
            match tokio::time::timeout_at(deadline, running.next()).await {
                Ok(Some(key)) => {
                    active.remove(&key);
                }
                _ => break,
            }
        }
        // Drop futures before recording their events so no canceled lane can append a late reply.
        for event in active.into_values().filter_map(|(_, event)| event) {
            self.dead_letter_event(
                &event,
                "node shut down while the event was being processed; its reply may be missing",
            )
            .await;
        }
        for event in self.simulation.drain() {
            self.dead_letter_event(
                &event,
                "node shut down before simulation consumed this event",
            )
            .await;
        }
        tracing::info!("Pipeline worker loop terminated");
    }

    /// Closes the ingest queue and records every event not yet started as a dead letter: first
    /// those already read ahead, then those still queued, so the log keeps arrival order.
    pub(super) async fn spill_queued_events(
        &self,
        waiting: &mut VecDeque<(String, IngestEventRequest)>,
        event_receiver: &mut mpsc::Receiver<IngestEventRequest>,
    ) {
        event_receiver.close();
        for (_, req) in waiting.drain(..) {
            if let Some(event) = req.event {
                self.dead_letter_event(&event, "node shut down before the event was processed")
                    .await;
            }
        }
        while let Some(req) = event_receiver.recv().await {
            if let Some(event) = req.event {
                self.dead_letter_event(&event, "node shut down before the event was processed")
                    .await;
            }
        }
    }

    /// Runs one ingested event through the pipeline and queues its replies for delivery.
    pub(super) async fn handle_ingested(&self, req: IngestEventRequest) {
        let event = match req.event {
            Some(evt) => evt,
            None => {
                tracing::warn!("Received IngestEventRequest with empty inner event; skipping");
                return;
            }
        };

        let platform = if !event.platform.is_empty() {
            event.platform.clone()
        } else {
            req.platform.clone()
        };
        let channel_id = event.channel_id.clone();
        let recipient_id = event.sender_id.clone();
        let event_id = event.event_id.clone();

        self.observe(PipelineStage::Ingested {
            event_id: event_id.clone(),
            platform: platform.clone(),
            channel_id: channel_id.clone(),
            sender_id: recipient_id.clone(),
        });

        tracing::info!(
            platform = %platform,
            channel_id = %channel_id,
            sender_id = %recipient_id,
            event_id = %event_id,
            "Pipeline received inbound event"
        );

        let result = self.process_event(event).await;

        match &result {
            PipelineResult::SimulationHandled => {
                tracing::info!(platform = %platform, channel_id = %channel_id, "Simulation accepted event; explicit actions control speech");
            }
            PipelineResult::LlmReplied { content, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    content_len = content.len(),
                    "Pipeline generated LLM reply"
                );
            }
            PipelineResult::LlmFailed { error, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    error = %error,
                    "Pipeline model turn failed"
                );
            }
            PipelineResult::CommandExecuted {
                command, success, ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    success = %success,
                    "Pipeline executed command"
                );
            }
            PipelineResult::Blocked { host_id, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    host_id = %host_id,
                    "Pipeline event blocked by PreFilter"
                );
            }
            PipelineResult::CommandNotFound { command } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline slash command not found"
                );
            }
            PipelineResult::SessionRotated {
                instance_id,
                session_id,
                ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    session_id = %session_id,
                    "Pipeline rotated conversation session via built-in /new"
                );
            }
            PipelineResult::ModelSelected {
                instance_id, model, ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    model = %model,
                    "Pipeline switched the instance model via built-in /model"
                );
            }
            PipelineResult::ModelListed {
                instance_id, count, ..
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    count = %count,
                    "Pipeline listed models via built-in /model"
                );
            }
            PipelineResult::ReplySuppressed {
                instance_id,
                reason,
            } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    instance_id = %instance_id,
                    reason = %reason,
                    "Pipeline suppressed a reply by policy"
                );
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "reply_policy",
                    reason: reason.clone(),
                });
            }
            PipelineResult::BuiltinReplied { command, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline answered a built-in informational command"
                );
            }
            PipelineResult::CommandDenied { command, .. } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    command = %command,
                    "Pipeline refused a command under the command policy"
                );
            }
            PipelineResult::Notice { kind, outcome } => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    notice = %kind,
                    outcome = %outcome,
                    "Pipeline handled a platform notice without answering"
                );
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "notice",
                    reason: outcome.clone(),
                });
            }
            PipelineResult::NoInstance { platform } => {
                // Already logged with the platform in `process_event`; nothing was delivered.
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "no_instance",
                    reason: format!("no enabled bot instance serves platform '{platform}'"),
                });
            }
            PipelineResult::Passed(_) => {
                tracing::info!(
                    platform = %platform,
                    channel_id = %channel_id,
                    "Pipeline event passed without a reply"
                );
                self.observe(PipelineStage::NoReply {
                    event_id: event_id.clone(),
                    cause: "nothing_to_say",
                    reason: "the pipeline produced nothing to send".to_string(),
                });
            }
        }

        let replies = result.replies();

        let split_lines = matches!(
            &result,
            PipelineResult::LlmReplied {
                split_lines: true,
                ..
            }
        );

        if !replies.is_empty() {
            self.enqueue_reply(
                DeliverMessageRequest {
                    platform,
                    channel_id,
                    recipient_id,
                    segments: replies.to_vec(),
                    event_id,
                },
                split_lines,
            );
        }
    }

    /// Hands a reply to the outbound queue without waiting.
    ///
    /// The pipeline worker must never await platform I/O; a full or closed queue sends the reply
    /// to the dead-letter log instead, written off the worker.
    pub(super) fn enqueue_reply(&self, deliver_req: DeliverMessageRequest, split_lines: bool) {
        let event_id = deliver_req.event_id.clone();
        let platform = deliver_req.platform.clone();
        let channel_id = deliver_req.channel_id.clone();
        let segment_count = deliver_req.segments.len();

        // Non-blocking hand-off: the pipeline worker must never await platform I/O.
        match self.outbound_sender.try_send(OutboundMessage {
            request: deliver_req,
            split_lines,
            receipt: None,
        }) {
            Ok(()) => {
                self.observe(PipelineStage::OutboundQueued {
                    event_id,
                    platform,
                    channel_id,
                    segment_count,
                });
            }
            Err(error) => {
                let (dropped, reason) = match error {
                    mpsc::error::TrySendError::Full(dropped) => (dropped, "outbound queue is full"),
                    mpsc::error::TrySendError::Closed(dropped) => {
                        (dropped, "outbound dispatcher is not running")
                    }
                };
                tracing::warn!(
                    platform = %platform,
                    channel_id = %channel_id,
                    reason,
                    "Outbound reply could not be queued; writing it to dead letter"
                );
                // The pipeline never waits on disk I/O for a reply it cannot send anyway.
                let dead_letter = Arc::clone(&self.dead_letter);
                tokio::spawn(async move {
                    if let Err(err) = dead_letter.write_record(&dropped.request, reason).await {
                        tracing::error!(error = %err, "Failed to persist dead letter record; the reply is lost");
                    }
                });
                self.observe(PipelineStage::OutboundFailed {
                    platform,
                    channel_id,
                    reason: format!("{reason}; reply dropped"),
                });
            }
        }
    }

    /// Shuts the pipeline down without losing anything it already accepted.
    ///
    /// Inbound first: the worker closes the ingest queue, records queued events and gets
    /// one shared [`SHUTDOWN_EVENT_GRACE`] for active events, whose replies still reach the outbound
    /// queue. Then outbound: queued replies are delivered for up to [`SHUTDOWN_DELIVERY_GRACE`] and
    /// the rest are recorded. Adapters and plugin hosts must stay up until this returns, because
    /// the final deliveries go through them.
    pub async fn drain(&self, worker: JoinHandle<()>, dispatcher: Option<JoinHandle<()>>) {
        self.shutdown.send_replace(ShutdownPhase::DrainingInbound);
        if let Err(err) = worker.await {
            tracing::error!(error = %err, "Pipeline worker failed during shutdown");
        }
        self.shutdown.send_replace(ShutdownPhase::DrainingOutbound(
            tokio::time::Instant::now() + SHUTDOWN_DELIVERY_GRACE,
        ));
        if let Some(dispatcher) = dispatcher
            && let Err(err) = dispatcher.await
        {
            tracing::error!(error = %err, "Outbound dispatcher failed during shutdown");
        }
    }

    /// Spawns the pipeline worker loop as a background Tokio task.
    pub fn start_worker(
        self: Arc<Self>,
        event_receiver: mpsc::Receiver<IngestEventRequest>,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run_worker_loop(event_receiver).await;
        })
    }
}
