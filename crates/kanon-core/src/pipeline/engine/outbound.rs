//! Bounded platform delivery and delivery receipts.

use super::*;

impl PipelineEngine {
    /// Tells subscribed plugins that the bot sent a message on `request.platform`.
    ///
    /// The plugins are those of the instance serving the platform, as for inbound events; with
    /// an instance registry but no instance claiming the platform nobody is told.
    pub(super) async fn emit_message_sent(
        &self,
        request: &DeliverMessageRequest,
        message_id: &str,
    ) {
        let hosts = self.supervisor.get_all_hosts().await;
        if !hosts.iter().any(|host| {
            host.metas()
                .iter()
                .any(|plugin| plugin.events().any(|kind| kind == EventKind::MessageSent))
        }) {
            return;
        }
        let instance = match &self.instances {
            Some(registry) => match registry.resolve_by_platform(&request.platform).await {
                Ok(Some(instance)) => Some(instance),
                _ => return,
            },
            None => None,
        };
        let hosts = self.instance_hosts(instance.as_ref(), hosts).await;
        hooks::emit_event(
            &hosts,
            EventKind::MessageSent,
            Detail::MessageSent(MessageSentEvent {
                message: Some(request.clone()),
                message_id: message_id.to_string(),
            }),
        );
    }

    /// Delivers one outbound message to the adapter owning its platform.
    ///
    /// Routing order is built-in adapter first, then plugin host. Every failure path is explicit:
    /// an unrouted platform, an unreachable plugin host, or a plugin that reports `success=false`
    /// all become [`AdapterError`] values instead of a silent drop.
    pub async fn deliver_outbound(
        &self,
        request: DeliverMessageRequest,
    ) -> Result<DeliveryOutcome, AdapterError> {
        let platform = request.platform.clone();

        match self.supervisor.resolve_adapter(&platform).await {
            Some(AdapterRoute::Builtin(adapter)) => {
                let response = adapter.deliver(request).await?;
                if !response.success {
                    return Err(AdapterError::Delivery {
                        platform,
                        reason: response.error_message,
                    });
                }
                Ok(DeliveryOutcome {
                    kind: AdapterKind::Builtin,
                    platform,
                    message_id: response.message_id,
                })
            }
            Some(AdapterRoute::Plugin { host, plugin_id }) => {
                let host_id = host.host_id.clone();
                let response = host.deliver_message(request).await.map_err(|status| {
                    AdapterError::Delivery {
                        platform: platform.clone(),
                        reason: format!(
                            "plugin '{plugin_id}' on host '{host_id}' failed: {status}"
                        ),
                    }
                })?;

                if !response.success {
                    return Err(AdapterError::Delivery {
                        platform,
                        reason: format!(
                            "plugin '{plugin_id}' on host '{host_id}' rejected the message: {}",
                            response.error_message
                        ),
                    });
                }

                Ok(DeliveryOutcome {
                    kind: AdapterKind::Plugin,
                    platform,
                    message_id: response.message_id,
                })
            }
            None => Err(AdapterError::UnknownPlatform(platform)),
        }
    }

    /// Delivers a single outbound message and publishes the resulting observation stage,
    /// protected by the platform's adaptive circuit breaker and cold-storage dead-letter queue.
    pub async fn dispatch_outbound_request(&self, request: DeliverMessageRequest) {
        let breaker = self.platform_circuit_breaker(&request.platform).await;
        self.dispatch_outbound_request_with_breaker(request, &breaker)
            .await;
    }

    /// Delivers a single outbound message using the given platform circuit breaker.
    ///
    /// # Fault Tolerance & Dead-Letter Persistence
    /// - If the breaker is `Open`, fast-skips the call, publishes [`PipelineStage::OutboundFailed`],
    ///   and appends the dropped message to `data/dead_letter/<platform>_<date>.jsonl`.
    /// - If delivery succeeds, records latency in the breaker and publishes [`PipelineStage::OutboundDelivered`].
    /// - If delivery fails, increments consecutive failures in the breaker (tripping if threshold reached),
    ///   persists the message to the dead-letter log, and publishes [`PipelineStage::OutboundFailed`].
    pub async fn dispatch_outbound_request_with_breaker(
        &self,
        request: DeliverMessageRequest,
        breaker: &CircuitBreaker,
    ) -> DeliverMessageResponse {
        let platform = request.platform.clone();
        let channel_id = request.channel_id.clone();
        let segment_count = request.segments.len();

        // Keep the reservation alive across delivery so cancellation cannot strand the
        // platform's sole half-open recovery probe.
        let Some(permit) = breaker.try_acquire() else {
            let reason = "platform circuit breaker open".to_string();
            tracing::warn!(
                platform = %platform,
                channel_id = %channel_id,
                "Platform circuit breaker tripped (OPEN); short-circuiting delivery and persisting to dead letter"
            );
            if let Err(err) = self.dead_letter.write_record(&request, &reason).await {
                tracing::error!(
                    platform = %platform,
                    error = %err,
                    "Failed to record dead-letter entry for short-circuited message"
                );
            }
            self.observe(PipelineStage::OutboundFailed {
                platform,
                channel_id,
                reason: "circuit breaker open, persisted to dead letter".to_string(),
            });
            return DeliverMessageResponse {
                success: false,
                message_id: String::new(),
                error_message: reason,
            };
        };

        let start = std::time::Instant::now();
        match self.deliver_outbound(request.clone()).await {
            Ok(outcome) => {
                let response = DeliverMessageResponse {
                    success: true,
                    message_id: outcome.message_id.clone(),
                    error_message: String::new(),
                };
                permit.success(start.elapsed());
                self.emit_message_sent(&request, &outcome.message_id).await;
                tracing::info!(
                    platform = %outcome.platform,
                    channel_id = %channel_id,
                    message_id = %outcome.message_id,
                    "Outbound message delivered"
                );
                self.observe(PipelineStage::OutboundDelivered {
                    platform: outcome.platform,
                    channel_id,
                    segment_count,
                    target: outcome.kind,
                    message_id: outcome.message_id,
                });
                response
            }
            Err(err) => {
                let reason = err.to_string();
                permit.failure(&reason);
                tracing::warn!(
                    platform = %platform,
                    channel_id = %channel_id,
                    error = %err,
                    "Outbound delivery failed; persisting to dead letter"
                );
                if let Err(write_err) = self.dead_letter.write_record(&request, &reason).await {
                    tracing::error!(
                        platform = %platform,
                        error = %write_err,
                        "Failed to persist dead letter record"
                    );
                }
                self.observe(PipelineStage::OutboundFailed {
                    platform,
                    channel_id,
                    reason: reason.clone(),
                });
                DeliverMessageResponse {
                    success: false,
                    message_id: String::new(),
                    error_message: reason,
                }
            }
        }
    }

    /// Spawns a dedicated sequential worker task for a single platform partition.
    pub(super) fn spawn_platform_worker(
        self: &Arc<Self>,
        platform: String,
        mut rx: mpsc::Receiver<OutboundMessage>,
    ) -> JoinHandle<()> {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            tracing::debug!(platform = %platform, "Platform outbound worker spawned");
            let breaker = engine.platform_circuit_breaker(&platform).await;
            let mut phase = engine.shutdown.subscribe();
            loop {
                // Workers retire after 30 seconds of inactivity to reclaim resources.
                match tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv()).await {
                    Ok(Some(message)) => {
                        // Do not start a queued reply after its caller has gone away.
                        // Already-started platform I/O is never retried on cancellation.
                        if message
                            .receipt
                            .as_ref()
                            .is_some_and(|receipt| receipt.is_closed())
                        {
                            continue;
                        }
                        // A complete answer occupies one queue slot. Expand it here, then finish
                        // all its parts before taking another answer to preserve platform FIFO.
                        let mut request = message.request;
                        let messages = if message.split_lines {
                            let limit = match engine.supervisor.adapters().get(&platform).await {
                                Some(adapter) => adapter.reply_message_limit(&request),
                                None => usize::MAX,
                            };
                            split_reply_lines(&std::mem::take(&mut request.segments), limit)
                        } else {
                            vec![std::mem::take(&mut request.segments)]
                        };
                        let mut messages = messages.into_iter();
                        let mut response = DeliverMessageResponse {
                            success: true,
                            ..Default::default()
                        };
                        while let Some(segments) = messages.next() {
                            let part = DeliverMessageRequest {
                                segments,
                                ..request.clone()
                            };
                            // Poll the deadline first for every part, including within a batch.
                            response = tokio::select! {
                                biased;
                                () = delivery_deadline_passed(&mut phase) => {
                                    engine.dead_letter_reply(
                                        &part,
                                        "node shut down before the reply was delivered; the platform may or may not have received it",
                                    ).await
                                }
                                response = engine.dispatch_outbound_request_with_breaker(
                                    part.clone(), &breaker,
                                ) => response,
                            };
                            if !response.success {
                                // The attempted part was already recorded. Persist only the
                                // unsent suffix: replaying the original batch duplicates its prefix.
                                let reason = format!(
                                    "preceding part of this reply failed: {}",
                                    response.error_message,
                                );
                                for segments in messages {
                                    engine
                                        .dead_letter_reply(
                                            &DeliverMessageRequest {
                                                segments,
                                                ..request.clone()
                                            },
                                            &reason,
                                        )
                                        .await;
                                }
                                break;
                            }
                        }
                        if let Some(receipt) = message.receipt {
                            let _ = receipt.send(response);
                        }
                    }
                    Ok(None) => {
                        // All channel senders dropped (shutting down).
                        break;
                    }
                    Err(_) => {
                        // Idle timeout reached; retire this worker.
                        tracing::debug!(platform = %platform, "Platform outbound worker idle timeout; retiring");
                        break;
                    }
                }
            }
        })
    }

    /// Records a reply that will not be delivered and reports the failure to its caller.
    pub(super) async fn dead_letter_reply(
        &self,
        request: &DeliverMessageRequest,
        reason: &str,
    ) -> DeliverMessageResponse {
        if let Err(err) = self.dead_letter.write_record(request, reason).await {
            tracing::error!(
                platform = %request.platform,
                channel_id = %request.channel_id,
                error = %err,
                "Failed to persist dead letter record; the reply is lost"
            );
        }
        self.observe(PipelineStage::OutboundFailed {
            platform: request.platform.clone(),
            channel_id: request.channel_id.clone(),
            reason: reason.to_string(),
        });
        DeliverMessageResponse {
            success: false,
            message_id: String::new(),
            error_message: reason.to_string(),
        }
    }

    /// Records an inbound event the node acknowledged but will never process.
    pub(super) async fn dead_letter_event(&self, event: &PipelineEventRequest, reason: &str) {
        if let Err(err) = self.dead_letter.write_inbound(event, reason).await {
            tracing::error!(
                platform = %event.platform,
                event_id = %event.event_id,
                error = %err,
                "Failed to persist dead letter record; the event is lost"
            );
        }
    }

    /// Reports an outbound drop when a specific platform's worker queue is saturated,
    /// and durably flushes the dropped message to the dead-letter queue log before returning.
    pub(super) async fn report_outbound_queue_full(&self, message: OutboundMessage) {
        let dropped = message.request;
        if let Some(receipt) = message.receipt {
            let _ = receipt.send(DeliverMessageResponse {
                success: false,
                message_id: String::new(),
                error_message: "platform outbound queue full".to_string(),
            });
        }
        let platform = dropped.platform.clone();
        let channel_id = dropped.channel_id.clone();
        tracing::warn!(
            platform = %platform,
            channel_id = %channel_id,
            "Platform outbound queue saturated; persisting message to dead letter to prevent cross-platform backpressure"
        );
        if let Err(err) = self
            .dead_letter
            .write_record(&dropped, "platform outbound queue saturated")
            .await
        {
            tracing::error!(
                platform = %platform,
                channel_id = %channel_id,
                error = %err,
                "Failed to persist dead letter record for saturated outbound queue"
            );
        }
        self.observe(PipelineStage::OutboundFailed {
            platform,
            channel_id,
            reason: "platform outbound queue full".to_string(),
        });
    }

    /// Drains the global outbound queue and partitions requests into per-platform sequential workers.
    ///
    /// # Concurrency & Ordering Guarantee
    /// - **Per-platform FIFO ordering**: Each platform has an independent sequential worker. Messages
    ///   for platform `A` are processed strictly in arrival order.
    /// - **Cross-platform isolation**: Platform `A` being slow, stalled, or failing never blocks
    ///   outbound deliveries to platform `B`.
    /// - **Bounded queue protection**: If platform `A`'s queue reaches capacity, overflow messages
    ///   are dropped and emit [`PipelineStage::OutboundFailed`], without stalling the global dispatcher.
    ///
    /// # Shutdown
    /// Once [`PipelineEngine::drain`] reaches the outbound phase, the queue is closed, the replies
    /// still in it are handed to their platform workers, and the dispatcher waits for every worker
    /// to finish. Workers deliver until [`SHUTDOWN_DELIVERY_GRACE`] runs out and record the rest.
    pub async fn run_outbound_loop(self: Arc<Self>, mut receiver: mpsc::Receiver<OutboundMessage>) {
        tracing::info!("Partitioned outbound adapter dispatcher started");
        let mut workers: HashMap<String, (mpsc::Sender<OutboundMessage>, JoinHandle<()>)> =
            HashMap::new();
        let mut phase = self.shutdown.subscribe();

        loop {
            let request = tokio::select! {
                biased;
                _ = wait_for_phase(&mut phase, |p| matches!(p, ShutdownPhase::DrainingOutbound(_))) => break,
                request = receiver.recv() => match request {
                    Some(request) => request,
                    None => break,
                },
            };
            self.route_outbound(request, &mut workers).await;
        }

        // Nothing new is accepted from here on; producers get an explicit `Closed` error.
        receiver.close();
        while let Some(request) = receiver.recv().await {
            self.route_outbound(request, &mut workers).await;
        }
        // Dropping each sender lets its worker finish the queue and stop; the delivery deadline
        // bounds how long that takes.
        for (platform, (sender, worker)) in workers {
            drop(sender);
            if let Err(err) = worker.await {
                tracing::error!(platform = %platform, error = %err, "Platform outbound worker failed during shutdown");
            }
        }
        tracing::info!("Partitioned outbound adapter dispatcher terminated");
    }

    /// Hands one reply to its platform's sequential worker, starting the worker if needed.
    pub(super) async fn route_outbound(
        self: &Arc<Self>,
        request: OutboundMessage,
        workers: &mut HashMap<String, (mpsc::Sender<OutboundMessage>, JoinHandle<()>)>,
    ) {
        let platform = request.request.platform.clone();

        // Periodic cleanup of dead channels to avoid memory buildup when platforms are dynamic
        if workers.len() > 128 {
            workers.retain(|_, (tx, _)| !tx.is_closed());
        }

        let tx = match workers.get(&platform) {
            Some((tx, _)) if !tx.is_closed() => tx.clone(),
            _ => {
                let (new_tx, rx) = mpsc::channel(DEFAULT_PLATFORM_QUEUE_CAPACITY);
                let worker = self.spawn_platform_worker(platform.clone(), rx);
                workers.insert(platform.clone(), (new_tx.clone(), worker));
                new_tx
            }
        };

        match tx.try_send(request) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Closed(req)) => {
                // The worker timed out immediately before the send; respawn and retry once
                let (new_tx, rx) = mpsc::channel(DEFAULT_PLATFORM_QUEUE_CAPACITY);
                let worker = self.spawn_platform_worker(platform.clone(), rx);
                workers.insert(platform, (new_tx.clone(), worker));
                if let Err(mpsc::error::TrySendError::Full(dropped)) = new_tx.try_send(req) {
                    self.report_outbound_queue_full(dropped).await;
                }
            }
            Err(mpsc::error::TrySendError::Full(dropped)) => {
                self.report_outbound_queue_full(dropped).await;
            }
        }
    }

    /// Spawns the outbound dispatcher task.
    ///
    /// Returns `None` when the dispatcher was already started: the queue has exactly one consumer,
    /// so a second call cannot be honoured and must not silently steal messages.
    pub fn start_outbound_dispatcher(self: Arc<Self>) -> Option<JoinHandle<()>> {
        let receiver = self
            .outbound_receiver
            .try_lock()
            .ok()
            .and_then(|mut guard| guard.take());

        let receiver = match receiver {
            Some(receiver) => receiver,
            None => {
                tracing::error!("Outbound dispatcher already running; ignoring duplicate start");
                return None;
            }
        };

        Some(tokio::spawn(async move {
            self.run_outbound_loop(receiver).await;
        }))
    }
}
