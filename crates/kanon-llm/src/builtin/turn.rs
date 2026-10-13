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
            .answer(
                session_id,
                media,
                hosts,
                options,
                &system_prompt,
                tools,
                None,
            )
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
        hosts: &[Arc<dyn ToolHost>],
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
                if stop.as_ref().is_some_and(StopSignal::is_stopped) {
                    return Err(AgentError::Stopped);
                }

                // Hook: tool call permission / safety check
                let mut permitted = true;
                for hook in &self.hooks {
                    if !hook.on_before_tool_call(session_id, &call).await? {
                        permitted = false;
                        break;
                    }
                }
                if !permitted {
                    tracing::info!(agent = %self.name, tool = %call.name, "Tool call was vetoed by agent hook");
                    let veto_msg =
                        format!("Tool '{}' execution was denied by agent policy", call.name);
                    self.memory
                        .push_message(session_id, ChatMessage::tool_response(&call.id, &veto_msg))
                        .await?;
                    executed_tools.push(ExecutedToolCall {
                        call_id: call.id,
                        tool_name: call.name.clone(),
                        plugin_id: "policy".to_string(),
                        host_id: "agent_hook".to_string(),
                        success: false,
                    });
                    continue;
                }

                // Validate once before either dispatch path. Dropping non-object arguments at
                // the protobuf boundary would turn a malformed call into an empty object; native
                // tools must receive the same contract. Keep the paired failure in history so
                // the model can correct its arguments on the next round.
                if !call.arguments.is_object() {
                    let err_msg = format!(
                        "Tool '{}' arguments must be a JSON object; correct the arguments and retry",
                        call.name
                    );
                    for hook in &self.hooks {
                        hook.on_after_tool_call(session_id, &call, &err_msg, false)
                            .await?;
                    }
                    self.memory
                        .push_message(session_id, ChatMessage::tool_response(&call.id, &err_msg))
                        .await?;
                    executed_tools.push(ExecutedToolCall {
                        call_id: call.id,
                        tool_name: call.name,
                        plugin_id: "validation".to_string(),
                        host_id: "agent".to_string(),
                        success: false,
                    });
                    if self.config.stop_on_tool_failure {
                        return Err(GatewayError::InvalidResponse(err_msg).into());
                    }
                    continue;
                }

                // Only dispatch a name advertised in this request, using the turn's original
                // target. A hook may hide a tool, but cannot alias it to a different provider.
                let target = tools
                    .targets
                    .get(&call.name)
                    .filter(|_| request.tools.iter().any(|tool| tool.name == call.name));

                // Branch A: the captured native tool index is stable for the agent's lifetime.
                if let Some(ToolTarget::Native(index)) = target {
                    let native_tool = &self.tools[*index];
                    tracing::debug!(agent = %self.name, tool = %call.name, "Executing native tool in-process");
                    let Some(result) = unless_stopped(
                        stop.as_ref(),
                        native_tool.call(session_id, call.arguments.clone()),
                    )
                    .await
                    else {
                        return Err(AgentError::Stopped);
                    };
                    let (result_str, is_success) = match result {
                        Ok(output) => {
                            // Same rule as plugin attachments: deduplicated, in execution
                            // order, and only from calls that succeeded.
                            for attachment in output.attachments {
                                if !attachments.contains(&attachment) {
                                    attachments.push(attachment);
                                }
                            }
                            (output.text, true)
                        }
                        Err(err) => (format!("Error: {err}"), false),
                    };

                    executed_tools.push(ExecutedToolCall {
                        call_id: call.id.clone(),
                        tool_name: call.name.clone(),
                        plugin_id: "native".to_string(),
                        host_id: "in_process".to_string(),
                        success: is_success,
                    });

                    for hook in &self.hooks {
                        hook.on_after_tool_call(session_id, &call, &result_str, is_success)
                            .await?;
                    }

                    self.memory
                        .push_message(
                            session_id,
                            ChatMessage::tool_response(&call.id, &result_str),
                        )
                        .await?;

                    if !is_success && self.config.stop_on_tool_failure {
                        return Err(AgentError::ToolFailed(format!(
                            "Native tool '{}' failed: {result_str}",
                            call.name
                        )));
                    }
                    continue;
                }

                // Branch B: send the original host-local name to its captured provider.
                let (target_host, plugin_id, actual_tool_name) = match target {
                    Some(ToolTarget::Plugin {
                        host_index,
                        plugin_id,
                        tool_name,
                    }) => (&hosts[*host_index], plugin_id, tool_name.clone()),
                    _ => {
                        tracing::warn!(tool = %call.name, "Requested tool not declared by any native tool or active host");
                        let err_msg = format!("Tool '{}' not registered", call.name);
                        // A call that was attempted is reported finished, failed, so observers
                        // can pair it with its start; the RPC-failure path below does the same.
                        // (A vetoed call never started, so it reports nothing.)
                        for hook in &self.hooks {
                            hook.on_after_tool_call(session_id, &call, &err_msg, false)
                                .await?;
                        }
                        self.memory
                            .push_message(
                                session_id,
                                ChatMessage::tool_response(&call.id, &err_msg),
                            )
                            .await?;
                        executed_tools.push(ExecutedToolCall {
                            call_id: call.id,
                            tool_name: call.name.clone(),
                            plugin_id: "unknown".to_string(),
                            host_id: "unknown".to_string(),
                            success: false,
                        });
                        if self.config.stop_on_tool_failure {
                            return Err(AgentError::ToolNotFound(call.name));
                        }
                        continue;
                    }
                };

                // Translate the validated object directly, without a string round trip.
                let structured_args = json_to_prost_struct(&call.arguments)
                    .expect("tool arguments were validated as an object");

                let tool_req = ToolCallRequest {
                    call_id: call.id.clone(),
                    tool_name: actual_tool_name,
                    session_id: session_id.to_string(),
                    payload: Some(tool_call_request::Payload::StructuredArgs(structured_args)),
                    // The host attaches the platform event of the turn; the agent never knew it.
                    context: None,
                };

                tracing::debug!(
                    agent = %self.name,
                    tool = %call.name,
                    host_id = %target_host.host_id(),
                    plugin_id = %plugin_id,
                    "Agent dispatching tool RPC to host"
                );

                let Some(result) =
                    unless_stopped(stop.as_ref(), target_host.call_tool(tool_req)).await
                else {
                    return Err(AgentError::Stopped);
                };
                match result {
                    Ok(resp) => {
                        // Decode before accepting attachments or recording success. A malformed
                        // result still needs a paired tool response so the session can continue.
                        let decoded = if !resp.success {
                            Err(resp.error_message)
                        } else {
                            match resp.payload {
                                Some(tool_call_response::Payload::StructuredResult(s)) => {
                                    prost_struct_to_json(s)
                                        .map(|value| value.to_string())
                                        .map_err(|error| {
                                            format!("invalid structured tool result: {error}")
                                        })
                                }
                                Some(tool_call_response::Payload::RawBytes(bytes)) => {
                                    Ok(String::from_utf8_lossy(&bytes).to_string())
                                }
                                None => Ok("{}".to_string()),
                            }
                        };
                        let is_success = decoded.is_ok();
                        let result_str = decoded.unwrap_or_else(|error| format!("Error: {error}"));
                        executed_tools.push(ExecutedToolCall {
                            call_id: call.id.clone(),
                            tool_name: call.name.clone(),
                            plugin_id: plugin_id.clone(),
                            host_id: target_host.host_id().to_string(),
                            success: is_success,
                        });

                        // Attachment paths are delivery metadata, not conversation content: they are
                        // collected here so the textual result can be redacted below. A filesystem
                        // path must never reach the model (or the transcript) through the tool result.
                        let redactions: Vec<String> = resp
                            .attachments
                            .iter()
                            .filter_map(|attachment| attachment.file_path.clone())
                            .collect();

                        // Attachments of a failed call are discarded with it: a partial image from
                        // an errored tool would be sent to the user as if it were a result.
                        if is_success {
                            for attachment in resp.attachments {
                                let attachment = ToolAttachment::from_proto(attachment);
                                if !attachments.contains(&attachment) {
                                    attachments.push(attachment);
                                }
                            }
                        }

                        let result_str = redact_tool_paths(result_str, &redactions);

                        for hook in &self.hooks {
                            hook.on_after_tool_call(session_id, &call, &result_str, is_success)
                                .await?;
                        }

                        self.memory
                            .push_message(
                                session_id,
                                ChatMessage::tool_response(&call.id, &result_str),
                            )
                            .await?;

                        if !is_success && self.config.stop_on_tool_failure {
                            tracing::warn!(tool = %call.name, "Tool reported failure and stop_on_tool_failure is enabled");
                            return Err(AgentError::ToolFailed(format!(
                                "Plugin tool '{}' failed: {result_str}",
                                call.name
                            )));
                        }
                    }
                    Err(status) => {
                        tracing::error!(tool = %call.name, error = %status, "Tool RPC execution failed");
                        executed_tools.push(ExecutedToolCall {
                            call_id: call.id.clone(),
                            tool_name: call.name.clone(),
                            plugin_id: plugin_id.clone(),
                            host_id: target_host.host_id().to_string(),
                            success: false,
                        });

                        let err_msg = format!("RPC Error: {}", status.message());
                        for hook in &self.hooks {
                            hook.on_after_tool_call(session_id, &call, &err_msg, false)
                                .await?;
                        }
                        self.memory
                            .push_message(session_id, ChatMessage::tool_response(&call.id, err_msg))
                            .await?;

                        if self.config.stop_on_tool_failure {
                            return Err(AgentError::from(status));
                        }
                    }
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
        let hosts = hosts.to_vec();
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        tokio::spawn(async move {
            // The producer, not the SSE reader, owns the writer through failure recovery and
            // the final commit. Tokio does not inherit task locals, so carry the stop explicitly.
            let _writing = writing;
            let turn = agent.answer(
                &session_id,
                media,
                &hosts,
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
