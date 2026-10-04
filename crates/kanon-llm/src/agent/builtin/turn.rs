//! Append-only reasoning rounds, tool execution and stream completion.

use super::*;

impl BuiltinAgent {
    /// Answers one turn: records the user message, then runs the reasoning and tool loop.
    ///
    /// # Flow:
    /// 1. Snapshots enabled tools and prepares matching static instructions;
    /// 2. Lets hooks enrich the user message, then records it in memory (without its media);
    /// 3. Runs the reasoning loop until the model emits final text or the iteration ceiling is hit,
    ///    invoking hooks around requests, tool authorizations and completions;
    /// 4. Dispatches tool calls (in-process for native tools, gRPC IPC for plugin tools);
    /// 5. Stores the assistant reply in memory and returns [`AgentOutput`].
    ///
    /// The textual projection in [`ChatMessage::content`] is what token accounting and summaries
    /// use, and the only part of the message stored in history: the images go out with this
    /// turn's requests alone.
    pub(super) async fn run_turn(
        &self,
        session_id: &str,
        message: ChatMessage,
        hosts: &[Arc<dyn ToolHost>],
        mut options: TurnOptions,
    ) -> Result<AgentOutput, AgentError> {
        options.persona = self.resolve_persona(session_id, options.persona)?;
        let tools = self.collect_tools(hosts, options.without_tools)?;
        let system_prompt = self
            .prepare_system_prompt(
                session_id,
                options.persona.as_ref(),
                &tools.definitions,
                options.instructions.as_deref(),
            )
            .await?;
        let media = self.start_turn(session_id, message).await?;

        // From here on the turn is part of history. However it ends, the next turn has to find a
        // conversation the provider accepts, and one that does not ask the model to redo the work
        // that just failed.
        match self
            .answer(session_id, media, options, &system_prompt, tools, None)
            .await
        {
            Ok(output) => Ok(output),
            Err(err) => Err(self.close_failed_turn(session_id, err).await),
        }
    }

    /// Enriches the message under the caller's task scope, then commits the turn once.
    pub(super) async fn start_turn(
        &self,
        session_id: &str,
        mut message: ChatMessage,
    ) -> Result<Option<Vec<ContentPart>>, AgentError> {
        for hook in &self.hooks {
            hook.on_user_message(session_id, &mut message).await?;
        }
        // A process can stop after persisting tool calls but before their results. Close that
        // interrupted round under this turn's writer before appending a new user message: results
        // appended after the new user would leave an invalid tool-call gap in durable history.
        let needs_recovery = {
            let history = self.memory.get_messages(session_id).await?;
            !unanswered_tool_calls(&history).is_empty()
        };
        if needs_recovery {
            self.record_closing(
                session_id,
                FAILED_TOOL_RESULT,
                "[The previous turn was interrupted before its tool calls completed. Unfinished \
                 calls were not retried. Answer the next message on its own, and redo the previous \
                 request only if the user asks for it again.]"
                    .to_string(),
            )
            .await?;
        }

        // 1. Push user message to memory, without its media. Platform media URLs are signed and
        // expire within hours (QQ's `rkey`), and a provider that cannot download one rejects the
        // whole request. Stored in append-only history, one dead URL would fail every later turn
        // of the session, so history keeps only the textual projection (`[图片]`) and the media
        // rides along with this turn's requests alone.
        let media = message.parts.take();
        self.memory.push_message(session_id, message).await?;

        Ok(media)
    }

    /// The reasoning and tool loop of [`BuiltinAgent::run_turn`], run once the user message is stored.
    pub(super) async fn answer(
        &self,
        session_id: &str,
        media: Option<Vec<ContentPart>>,
        options: TurnOptions,
        system_prompt: &str,
        tools: TurnTools,
        stream: Option<&tokio::sync::mpsc::Sender<Result<ChatChunk, AgentError>>>,
    ) -> Result<AgentOutput, AgentError> {
        // Model capability and per-turn policy share one execution boundary. The same decision
        // controls advertisement and dispatch, including calls introduced by middleware.
        let tools_enabled = self.config.tool_calling && !options.without_tools;

        let max_iterations = options.max_iterations.unwrap_or(self.config.max_iterations);

        let mut executed_tools = Vec::new();
        let mut attachments: Vec<ToolAttachment> = Vec::new();
        let mut reasoning: Vec<String> = Vec::new();
        let mut iterations = 0;
        // Commit once when the turn completes, retaining the existing convention that failed
        // or stopped turns do not increment session tallies. Every completed model round costs
        // tokens, including tool rounds and the response that reaches the iteration ceiling.
        let mut turn_tokens = 0usize;
        // Set when the caller can stop this turn from outside (see `crate::stop`).
        let stop = crate::stop::current();

        // 3. Reasoning and tool execution loop
        loop {
            let mut request = self
                .build_request(
                    session_id,
                    tools_enabled.then_some(tools.definitions.as_slice()),
                    system_prompt,
                )
                .await?;
            attach_turn_media(&mut request, media.as_ref());

            let mut response = self.complete_round(&request, stream, stop.as_ref()).await?;
            response.separate_reasoning();
            turn_tokens += response.usage.as_ref().map_or_else(
                || {
                    crate::token::estimate_request_tokens(&request)
                        + if response.has_assistant_payload() {
                            crate::token::estimate_message_tokens(&response.assistant_message())
                        } else {
                            0
                        }
                },
                |usage| usage.total_tokens as usize,
            );

            // Recover tool calls a model emitted as text markup instead of structured calls, so the
            // loop executes them instead of sending the markup to the chat platform as an answer.
            let had_structured_calls = !response.tool_calls.is_empty();
            normalize_textual_tool_calls(&mut response)?;
            if stream.is_some() && !had_structured_calls && !response.tool_calls.is_empty() {
                // Text has already been delivered. Never reinterpret it as an executable command
                // afterward; providers used for streaming must emit structured tool calls.
                return Err(GatewayError::InvalidResponse(
                    "streaming requires structured tool calls; textual tool markup was not executed".into(),
                ).into());
            }

            // Streaming is append-only delivery as well: an observer may inspect the complete
            // response, but cannot retroactively replace text already delivered to the client.
            let streamed_text =
                stream.map(|_| (response.content.clone(), response.reasoning_content.clone()));
            for hook in &self.hooks {
                hook.on_llm_response(session_id, &mut response).await?;
            }
            if let Some((content, reasoning)) = streamed_text
                && (response.content != content || response.reasoning_content != reasoning)
            {
                return Err(AgentError::InvalidRequest(
                    "response hooks cannot rewrite text already streamed".into(),
                ));
            }

            // Hidden definitions alone are not an execution policy: a model can replay an old
            // call or emit textual markup, and a response hook can introduce a new call.
            if !tools_enabled && !response.tool_calls.is_empty() {
                return Err(GatewayError::InvalidResponse(
                    "model returned tool calls while tools are disabled for this turn".into(),
                )
                .into());
            }

            response.separate_reasoning();
            if let Some(text) = response
                .reasoning_content
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                reasoning.push(text.to_string());
            }

            // Terminal state: Model completed generation without requesting tools
            if response.tool_calls.is_empty() {
                let reply = response.assistant_message();
                if response.has_assistant_payload() {
                    self.memory.push_message(session_id, reply.clone()).await?;
                }
                let final_content = response.content.clone().unwrap_or_default();

                if let Some(ref sm) = self.session_manager {
                    sm.record_turn(session_id, turn_tokens);
                }

                self.schedule_compaction(session_id, &request, response.usage.as_ref(), &reply)
                    .await;

                return Ok(AgentOutput {
                    content: final_content,
                    reasoning: joined_reasoning(&reasoning),
                    executed_tools,
                    turns: iterations + 1,
                    finish_reason: response.finish_reason,
                    attachments,
                });
            }

            // Guard against runaway loop recursion
            if iterations >= max_iterations {
                tracing::warn!(
                    agent = %self.name,
                    session_id = %session_id,
                    iterations = iterations,
                    "Agent reasoning iteration ceiling reached; breaking loop"
                );
                let fallback = response
                    .content
                    .clone()
                    .filter(|text| !text.is_empty())
                    .unwrap_or_else(|| {
                        "Agent reasoning recursion limit reached; execution terminated.".to_string()
                    });
                if let Some(output) = stream
                    && response.content.as_deref().unwrap_or_default().is_empty()
                {
                    let _ =
                        unless_stopped(stop.as_ref(), output.send(Ok(ChatChunk::delta(&fallback))))
                            .await
                            .ok_or(AgentError::Stopped)?;
                }
                let mut message = ChatMessage::assistant(&fallback);
                message.reasoning_content = response.reasoning_content;
                self.memory
                    .push_message(session_id, message.clone())
                    .await?;

                if let Some(ref sm) = self.session_manager {
                    sm.record_turn(session_id, turn_tokens);
                }

                self.schedule_compaction(session_id, &request, None, &message)
                    .await;

                return Ok(AgentOutput {
                    content: fallback,
                    reasoning: joined_reasoning(&reasoning),
                    executed_tools,
                    turns: iterations + 1,
                    finish_reason: Some("max_iterations".to_string()),
                    attachments,
                });
            }

            iterations += 1;

            // Record assistant tool calls turn into history
            self.memory
                .push_message(session_id, response.assistant_message())
                .await?;

            // Execute each requested tool call. A turn that ends inside the round leaves the calls
            // after it unanswered; `close_failed_turn` gives each of them a result.
            for call in response.tool_calls {
                let output = tools
                    .execute(session_id, &call, &request.tools, &self.hooks)
                    .await?;
                for attachment in output.attachments {
                    if !attachments.contains(&attachment) {
                        attachments.push(attachment);
                    }
                }
                executed_tools.push(output.record);
                self.memory
                    .push_message(
                        session_id,
                        ChatMessage::tool_response(&call.id, &output.text),
                    )
                    .await?;
                if self.config.stop_on_tool_failure
                    && let Some(error) = output.failure
                {
                    return Err(error);
                }
            }
        }
    }

    /// Closes a turn that ended without an answer and returns the error it ended with.
    ///
    /// Two things would otherwise break the session's later turns. A tool call without a result
    /// makes providers reject every later request of the session, so each open call of the last
    /// round gets one. And a turn that just ends leaves the user's request open at the end of
    /// history: the next turn's model sees it unanswered and sets out to do that work again, so
    /// work that failed once (a long program the model could not finish writing in time) fails the
    /// same way on every message that follows. A closing note in the assistant's place records
    /// that the turn ended and was not retried; the next message is answered on its own, and
    /// whether to try again is the user's call.
    ///
    /// Failing to record the closing is logged and does not replace the turn's own error, which is
    /// what the caller acts on.
    pub(super) async fn close_failed_turn(&self, session_id: &str, err: AgentError) -> AgentError {
        let tool_result = match err {
            AgentError::Stopped => STOPPED_TOOL_RESULT,
            _ => FAILED_TOOL_RESULT,
        };
        if let Err(close_err) = self
            .record_closing(session_id, tool_result, closing_note(&err))
            .await
        {
            tracing::error!(
                agent = %self.name,
                session_id = %session_id,
                error = %close_err,
                "Could not record the end of an unanswered turn; the session's next request may fail"
            );
        }
        tracing::info!(
            agent = %self.name,
            session_id = %session_id,
            error = %err,
            "Turn ended without an answer; closed in history and not retried"
        );
        err
    }

    /// Answers the open tool calls of the last round and appends the closing note.
    pub(super) async fn record_closing(
        &self,
        session_id: &str,
        tool_result: &str,
        note: String,
    ) -> Result<(), MemoryError> {
        let history = self.memory.get_messages(session_id).await?;
        let mut closing: Vec<ChatMessage> = unanswered_tool_calls(&history)
            .into_iter()
            .map(|id| ChatMessage::tool_response(id, tool_result))
            .collect();
        closing.push(ChatMessage::assistant(note));
        // Transactional backends must retain the old interrupted round if any closing write fails.
        self.memory.extend_messages(session_id, closing).await
    }

    /// Reads one model round, forwarding only text deltas; the shared loop handles complete tools.
    pub(super) async fn complete_round(
        &self,
        request: &ChatRequest,
        stream: Option<&tokio::sync::mpsc::Sender<Result<ChatChunk, AgentError>>>,
        stop: Option<&StopSignal>,
    ) -> Result<ChatResponse, AgentError> {
        let Some(output) = stream else {
            return unless_stopped(stop, self.provider.chat(request))
                .await
                .ok_or(AgentError::Stopped)?
                .map_err(AgentError::from);
        };
        let inner = unless_stopped(stop, self.provider.chat_stream(request))
            .await
            .ok_or(AgentError::Stopped)??;
        let mut inner = crate::gateway::reasoning::separate_stream(inner);
        let mut response = ChatResponse {
            content: Some(String::new()),
            ..ChatResponse::default()
        };
        loop {
            let next = unless_stopped(stop, inner.next())
                .await
                .ok_or(AgentError::Stopped)?;
            let Some(chunk) = next else {
                return Err(GatewayError::InvalidResponse(
                    "model stream ended before its terminal marker".into(),
                )
                .into());
            };
            let mut chunk = chunk?;
            if let Some(reasoning) = &chunk.reasoning_text {
                response
                    .reasoning_content
                    .get_or_insert_default()
                    .push_str(reasoning);
            }
            response
                .content
                .get_or_insert_default()
                .push_str(&chunk.delta_text);
            response.tool_calls.append(&mut chunk.tool_calls);
            let finished = chunk.is_finished;
            if finished {
                response.finish_reason = chunk.finish_reason.take();
                chunk.is_finished = false;
            }
            if !chunk.delta_text.is_empty() || chunk.reasoning_text.is_some() {
                // A disconnected reader does not cancel committed work: finish the model/tool
                // round and persist it before releasing its writer. Explicit /stop still cancels.
                let _ = unless_stopped(stop, output.send(Ok(chunk)))
                    .await
                    .ok_or(AgentError::Stopped)?;
            }
            if finished {
                break;
            }
        }
        Ok(response)
    }

    /// Runs the same turn state machine with streamed model rounds and one durable completion.
    pub(super) async fn stream_turn(
        &self,
        session_id: &str,
        user_input: &str,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<ChatChunkStream<AgentError>, AgentError> {
        let writing = self.turn_writer(session_id)?;
        let options = TurnOptions {
            persona: self.resolve_persona(session_id, None)?,
            ..TurnOptions::default()
        };
        let tools = self.collect_tools(hosts, options.without_tools)?;
        let system_prompt = self
            .prepare_system_prompt(
                session_id,
                options.persona.as_ref(),
                &tools.definitions,
                options.instructions.as_deref(),
            )
            .await?;
        let media = self
            .start_turn(session_id, ChatMessage::user(user_input))
            .await?;
        let stop = crate::stop::current();
        let agent = self.clone();
        let session_id = session_id.to_string();
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            // The producer, not the SSE reader, owns the writer through failure recovery and
            // the final commit. Tokio does not inherit task locals, so carry the stop explicitly.
            let _writing = writing;
            let turn = agent.answer(
                &session_id,
                media,
                options,
                &system_prompt,
                tools,
                Some(&tx),
            );
            let result = match stop {
                Some(stop) => crate::with_stop_signal(stop, turn).await,
                None => turn.await,
            };
            match result {
                Ok(output) => {
                    let _ = tx.send(Ok(ChatChunk::done(output.finish_reason))).await;
                }
                Err(error) => {
                    let error = agent.close_failed_turn(&session_id, error).await;
                    let _ = tx.send(Err(error)).await;
                }
            }
        });
        Ok(Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx)))
    }
}
