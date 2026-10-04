//! Plugin lifecycle dispatch over the router declarations.

use super::*;

#[async_trait]
impl Plugin for Router {
    fn meta(&self) -> PluginMeta {
        let mut meta = self.meta.clone();
        meta.tools = self.context.tool_metas();
        meta
    }

    async fn on_load(&mut self, ctx: &mut PluginContext) -> PluginResult<()> {
        let ctx = ctx.clone();
        self.context.update(|slot| *slot = Some(ctx));
        Ok(())
    }

    async fn on_config_reload(&mut self, config: prost_types::Struct) -> PluginResult<()> {
        self.context.update(|slot| {
            if let Some(ctx) = slot {
                ctx.config = Some(config);
            }
        });
        Ok(())
    }

    async fn on_pre_filter(
        &self,
        req: PipelineEventRequest,
    ) -> PluginResult<Option<PreFilterResult>> {
        match &self.pre_filter {
            Some(handler) => handler(MessageEvent::new(req, self.context.handle())).await,
            None => Ok(None),
        }
    }

    /// Runs a command, trigger or continuation and answers once the handler yields.
    ///
    /// A continuation resumes the handler suspended in `wait_next` for this conversation; if
    /// none is waiting (the host restarted, or the wait already timed out), the handler named
    /// by `req.command` is called afresh with `continuation` set.
    async fn on_execute_command(
        &self,
        req: CommandExecuteRequest,
    ) -> PluginResult<CommandExecuteResponse> {
        if req.continuation {
            let key =
                MessageEvent::new(req.context.clone().unwrap_or_default(), None).conversation_key();
            if let Some(waiter) = self.conversations.take(&key) {
                let done = waiter.session.open_turn();
                let event = CommandEvent::new(
                    req.clone(),
                    self.context.handle(),
                    Some(waiter.session.clone()),
                );
                if waiter.resume.send(event).is_ok() {
                    return Ok(settle(done).await);
                }
                // The handler stopped waiting in between; run the command afresh instead.
                waiter.session.abandon_turn();
            }
        }

        match self.commands.get(&req.command) {
            Some(handler) => Ok(self.start(handler.clone(), req).await),
            None => Ok(CommandExecuteResponse {
                success: false,
                error_message: format!("Unknown command: {}", req.command),
                ..Default::default()
            }),
        }
    }

    async fn on_call_tool(&self, req: ToolCallRequest) -> PluginResult<ToolCallResponse> {
        let Some(handler) = self.context.tool_handler(&req.tool_name) else {
            return Ok(ToolCallResponse {
                call_id: req.call_id,
                success: false,
                error_message: format!("Unknown tool: {}", req.tool_name),
                ..Default::default()
            });
        };
        let args = match req.payload {
            Some(tool_call_request::Payload::StructuredArgs(args)) => {
                match crate::json::from_struct(args) {
                    Ok(args) => serde_json::Value::Object(args),
                    Err(error) => {
                        return Ok(ToolCallResponse {
                            call_id: req.call_id,
                            success: false,
                            error_message: format!("invalid tool arguments: {error}"),
                            ..Default::default()
                        });
                    }
                }
            }
            _ => serde_json::Value::Object(serde_json::Map::new()),
        };
        let event = self.message(req.context);
        Ok(match handler(args, event).await {
            Ok((value, attachments)) => ToolCallResponse {
                call_id: req.call_id,
                success: true,
                payload: Some(tool_call_response::Payload::StructuredResult(
                    result_struct(value),
                )),
                attachments,
                ..Default::default()
            },
            // The model is told the tool failed and can explain or retry.
            Err(err) => ToolCallResponse {
                call_id: req.call_id,
                success: false,
                error_message: err.to_string(),
                ..Default::default()
            },
        })
    }

    async fn on_invoke_action(
        &self,
        action: &str,
        params: serde_json::Value,
    ) -> PluginResult<serde_json::Value> {
        match self.actions.get(action) {
            Some(handler) => handler(params).await,
            None => Err(format!(
                "unknown action '{action}'; declared actions: {:?}",
                self.actions.keys().collect::<Vec<_>>()
            )
            .into()),
        }
    }

    async fn on_event(&self, notification: EventNotification) -> PluginResult<()> {
        use event_notification::Detail;
        let (kind, event) = match notification.detail {
            Some(Detail::MessageSent(sent)) => (EventKind::MessageSent, Event::MessageSent(sent)),
            Some(Detail::Notice(notice)) => (
                EventKind::Notice,
                Event::Notice(MessageEvent::new(notice, self.context.handle())),
            ),
            Some(Detail::LlmResponse(answer)) => {
                (EventKind::LlmResponse, Event::LlmResponse(answer))
            }
            Some(Detail::AgentBegin(begin)) => (
                EventKind::AgentBegin,
                Event::AgentBegin(AgentBegin {
                    event: self.message(begin.context),
                    session_id: begin.session_id,
                }),
            ),
            Some(Detail::AgentDone(done)) => (
                EventKind::AgentDone,
                Event::AgentDone(AgentDone {
                    event: self.message(done.context),
                    session_id: done.session_id,
                    success: done.success,
                    content: done.content,
                    error: done.error,
                    tools: done.tools,
                }),
            ),
            Some(Detail::ToolCall(call)) => (
                EventKind::ToolCall,
                Event::ToolCall(ToolCall {
                    event: self.message(call.context),
                    session_id: call.session_id,
                    tool_name: call.tool_name,
                    arguments: serde_json::Value::Object(
                        call.arguments
                            .map(crate::json::from_struct)
                            .transpose()?
                            .unwrap_or_default(),
                    ),
                }),
            ),
            Some(Detail::ToolResult(result)) => (
                EventKind::ToolResult,
                Event::ToolResult(ToolResult {
                    event: self.message(result.context),
                    session_id: result.session_id,
                    tool_name: result.tool_name,
                    success: result.success,
                    result: result.result,
                }),
            ),
            // A notification always names its kind; an empty one is a core or version bug.
            None => return Err("event notification without a detail".into()),
        };
        for (_, handler) in self.events.iter().filter(|(k, _)| *k == kind) {
            // One failing subscriber must not stop the others.
            if let Err(err) = handler(event.clone()).await {
                tracing::warn!(?kind, error = %err, "Event handler failed");
            }
        }
        Ok(())
    }

    async fn on_decorate_reply(
        &self,
        req: DecorateReplyRequest,
    ) -> PluginResult<Option<Vec<MessageSegment>>> {
        let Some(handler) = &self.decorator else {
            return Ok(None);
        };
        handler(Reply {
            event: MessageEvent::new(req.context.unwrap_or_default(), self.context.handle()),
            segments: req.segments,
            source: ReplySource::try_from(req.source).unwrap_or(ReplySource::Unspecified),
            command: req.command,
        })
        .await
    }

    async fn on_prepare_turn(&self, req: PrepareTurnRequest) -> PluginResult<String> {
        let Some(handler) = &self.preparer else {
            return Ok(String::new());
        };
        handler(
            MessageEvent::new(req.context.unwrap_or_default(), self.context.handle()),
            req.session_id,
        )
        .await
    }

    async fn on_llm_request(&self, req: LlmRequestHookRequest) -> PluginResult<Option<String>> {
        let Some(handler) = &self.prompt_rewriter else {
            return Ok(None);
        };
        handler(SystemPrompt {
            event: MessageEvent::new(req.context.unwrap_or_default(), self.context.handle()),
            session_id: req.session_id,
            prompt: req.system_prompt,
        })
        .await
    }

    async fn on_http_request(&self, req: HttpRequest) -> PluginResult<HttpResponse> {
        Ok(self.http.dispatch(req).await)
    }
}

/// Converts a handler's JSON result into the `Struct` the wire carries.
pub(super) fn result_struct(value: serde_json::Value) -> prost_types::Struct {
    match value {
        serde_json::Value::Object(fields) => crate::json::to_struct(fields),
        other => {
            crate::json::to_struct(serde_json::Map::from_iter([("result".to_string(), other)]))
        }
    }
}
