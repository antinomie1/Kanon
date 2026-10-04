//! Kanon's built-in agent engine.
//!
//! [`BuiltinAgent`] is the node's implementation of [`Agent`]: a tool loop over one
//! [`LlmProvider`], with
//! - conversational memory implementing [`Memory`] (append-only, compacted into summaries);
//! - native in-process tools implementing [`AgentTool`] (zero IPC overhead);
//! - cross-process tool calling via [`ToolHost`];
//! - static persona composition and lifecycle hooks for skills, guardrails and tracing.

use async_trait::async_trait;
use std::sync::Arc;

use kanon_proto::v1::{ToolCallRequest, tool_call_request, tool_call_response};

use crate::agent::{Agent, AgentConfig, AgentHook, AgentOutput, AgentTool, TurnOptions};
use crate::compaction::{COMPACTION_INSTRUCTION, CompactionPolicy, ends_cleanly, summary_block};
use crate::error::{AgentError, GatewayError, MemoryError};
use crate::gateway::types::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, Role, TokenUsage, ToolDefinition,
};
use crate::gateway::{ChatChunk, ChatChunkStream, LlmProvider};
use crate::layout::normalize_request;
use crate::memory::{InMemory, Memory, MemorySnapshot};
use crate::prompt::Persona;
use crate::session::{SessionWriteGuard, SessionWriters};
use crate::stop::{StopSignal, unless_stopped};
use crate::tool_router::{
    ExecutedToolCall, ToolAttachment, ToolHost, ensure_unique_tool_names, json_to_prost_struct,
    prost_struct_to_json, resolve_tools,
};
use dashmap::DashSet;
use tokio_stream::StreamExt;

/// Result recorded for a tool call that a stopped turn left without one.
///
/// Worded for the model, which reads it in later turns: the call produced nothing because the
/// turn was stopped, not because the tool failed.
pub const STOPPED_TOOL_RESULT: &str =
    "Not completed: the turn was stopped before this tool call finished.";

/// Result recorded for a tool call left without one when its turn failed.
pub const FAILED_TOOL_RESULT: &str =
    "Not completed: the turn failed before this tool call finished.";

/// Kanon's own agent: a tool loop over one model provider.
///
/// Encapsulates model backend, memory store, native tools, lifecycle hooks, and reasoning policies.
///
/// Cloning is cheap (every heavy part is behind an `Arc`) and is how a conversation compaction
/// outlives the turn that scheduled it: the clone shares the memory, provider and hooks.
#[derive(Clone)]
pub struct BuiltinAgent {
    /// Identifier or role name of this agent.
    name: String,
    /// Static instructions placed after the persona and before ordinary lifecycle hooks.
    system_prompt: Option<String>,
    /// Provider backend for LLM completions.
    provider: Arc<dyn LlmProvider>,
    /// Pluggable memory backend for conversational history.
    memory: Arc<dyn Memory>,
    /// Optional session manager tracking lifecycle, turns, and metadata.
    session_manager: Option<Arc<crate::session::SessionManager>>,
    /// Optional persona registry for dynamic persona resolution.
    persona_registry: Option<Arc<crate::prompt::PersonaRegistry>>,
    /// Native in-process tools directly callable without IPC overhead.
    tools: Vec<Arc<dyn AgentTool>>,
    /// Lifecycle interception hooks.
    hooks: Vec<Arc<dyn AgentHook>>,
    /// Operational configuration.
    config: AgentConfig,
    /// Standalone turns, stream producers and compactions share one writer registry. Managed
    /// agents use the session manager's registry and need no second lock layer.
    standalone_writers: Option<Arc<SessionWriters>>,
    /// Sessions with a compaction in flight, so a burst of turns schedules one, not many.
    compacting: Arc<DashSet<String>>,
}

/// A completed turn's exact history and effective system prefix, captured while it owns the writer.
struct PreparedCompaction {
    snapshot: MemorySnapshot,
    prefix: Vec<ChatMessage>,
}

/// The execution target captured with the definition advertised for this turn.
enum ToolTarget {
    Native(usize),
    Plugin {
        host_index: usize,
        plugin_id: String,
        tool_name: String,
    },
}

/// Definitions and dispatch share one snapshot even when a host refreshes its metadata mid-turn.
#[derive(Default)]
struct TurnTools {
    definitions: Vec<ToolDefinition>,
    targets: std::collections::HashMap<String, ToolTarget>,
}

impl BuiltinAgent {
    /// Returns a new fluent builder for constructing a [`BuiltinAgent`].
    pub fn builder(name: impl Into<String>, provider: Arc<dyn LlmProvider>) -> AgentBuilder {
        AgentBuilder::new(name, provider)
    }

    /// Reference to the optional persona registry.
    pub fn persona_registry(&self) -> Option<&Arc<crate::prompt::PersonaRegistry>> {
        self.persona_registry.as_ref()
    }

    /// Registered native tools.
    pub fn tools(&self) -> &[Arc<dyn AgentTool>] {
        &self.tools
    }

    /// Registered lifecycle hooks.
    pub fn hooks(&self) -> &[Arc<dyn AgentHook>] {
        &self.hooks
    }

    /// Claims this turn's writer before any history append, preserving managed delegation.
    fn turn_writer(&self, session_id: &str) -> Result<SessionWriteGuard, AgentError> {
        match &self.session_manager {
            Some(sessions) => sessions.agent_write(session_id),
            None => self
                .standalone_writers
                .as_ref()
                .expect("standalone agents initialize their writer registry")
                .try_write(session_id),
        }
        .map_err(Into::into)
    }

    /// Merges native and plugin/MCP tools; final normalization follows the request hooks.
    fn collect_tools(&self, hosts: &[Arc<dyn ToolHost>]) -> Result<TurnTools, AgentError> {
        let mut tools = TurnTools::default();
        for (index, native) in self.tools.iter().enumerate() {
            let definition = native.definition();
            tools
                .targets
                .insert(definition.name.clone(), ToolTarget::Native(index));
            tools.definitions.push(definition);
        }
        for resolved in resolve_tools(hosts)? {
            tools.targets.insert(
                resolved.definition.name.clone(),
                ToolTarget::Plugin {
                    host_index: resolved.host_index,
                    plugin_id: resolved.plugin_id,
                    tool_name: resolved.tool_name,
                },
            );
            tools.definitions.push(resolved.definition);
        }
        // Never publish a map in which insertion silently selected one of two same-named tools.
        ensure_unique_tool_names(tools.definitions.iter().map(|tool| tool.name.as_str()))?;
        Ok(tools)
    }

    /// Resolves one immutable persona before recording a turn or beginning manual compaction.
    ///
    /// Instance inheritance overrides an explicit session choice without mutating it. Unknown
    /// bindings fail before history changes; tool rounds never look up a different persona later.
    fn resolve_persona(
        &self,
        session_id: &str,
        inherited: Option<Persona>,
    ) -> Result<Option<Persona>, AgentError> {
        if let Some(persona) = inherited {
            persona
                .validate()
                .map_err(|error| AgentError::InvalidRequest(error.to_string()))?;
            return Ok(Some(persona));
        }
        let Some(sessions) = &self.session_manager else {
            return Ok(None);
        };
        let Some(bound) = sessions.get_persona(session_id) else {
            return Ok(self
                .persona_registry
                .as_ref()
                .map(|personas| personas.base()));
        };
        let personas = self.persona_registry.as_ref().ok_or_else(|| {
            AgentError::InvalidRequest(format!(
                "session '{session_id}' is bound to persona '{bound}' without a persona catalog"
            ))
        })?;
        personas.get(&bound).map(Some).ok_or_else(|| {
            AgentError::InvalidRequest(format!(
                "session '{session_id}' is bound to missing persona '{bound}'"
            ))
        })
    }

    /// Prepares the static persona, skill and plugin text once, under the caller's turn scope.
    ///
    /// The returned string belongs to this turn, so catalog edits and other sessions' cache
    /// eviction cannot alter it. Stream producers carry it explicitly across their task boundary.
    async fn prepare_system_prompt(
        &self,
        session_id: &str,
        persona: Option<&Persona>,
    ) -> Result<String, AgentError> {
        let mut parts = Vec::new();
        if let Some(persona) = persona {
            parts.push(ChatMessage::system(persona.prompt.clone()));
        }
        if let Some(prompt) = &self.system_prompt {
            parts.push(ChatMessage::system(prompt.clone()));
        }
        let mut prompt = crate::layout::system_text(&parts);
        for hook in &self.hooks {
            hook.on_system_prompt(session_id, &mut prompt).await?;
        }
        Ok(prompt)
    }

    /// Builds the request for one model call, laid out static-first.
    ///
    /// The messages start as the session history (append-only). The agent's own instructions go
    /// after the resolved persona, hooks then add the skill catalog, and the summary (when there is
    /// one) follows them, and [`normalize_request`] merges that whole static block into one system
    /// message. The result is `[tools] [system] [history…] [current turn]`: only the tail changes
    /// from one call to the next, so the provider serves everything before it from its prompt cache.
    async fn build_request(
        &self,
        session_id: &str,
        tools: Option<&[ToolDefinition]>,
        system_prompt: &str,
    ) -> Result<ChatRequest, AgentError> {
        let snapshot = self.memory.snapshot(session_id).await?;
        self.request_from(session_id, snapshot, tools, system_prompt, None)
            .await
    }

    /// Builds a request from an already-read memory snapshot.
    ///
    /// Compaction reads the snapshot once and builds from it, so the messages it later folds away
    /// are exactly the ones its summary was written from. `None` disables tools, including any
    /// schemas added by hooks; an empty slice still allows ordinary request middleware.
    async fn request_from(
        &self,
        session_id: &str,
        snapshot: MemorySnapshot,
        tools: Option<&[ToolDefinition]>,
        system_prompt: &str,
        prepared_prefix: Option<&[ChatMessage]>,
    ) -> Result<ChatRequest, AgentError> {
        let MemorySnapshot { summary, messages } = snapshot;

        let mut request = ChatRequest {
            model: self.config.default_model.clone(),
            messages,
            tools: tools.unwrap_or_default().to_vec(),
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
        };

        if let Some(prefix) = prepared_prefix {
            // Automatic compaction must reuse the final prefix, including plugin rewrites and
            // its summary, even if configuration or a hook's cache changed after this turn.
            request.messages.splice(..0, prefix.iter().cloned());
        } else {
            if !system_prompt.is_empty() {
                request
                    .messages
                    .insert(0, ChatMessage::system(system_prompt));
            }

            // Ordinary request hooks still run for every model round; static preparation is
            // already complete, so tracing and request-specific middleware keep their contract.
            for hook in &self.hooks {
                hook.on_llm_request(session_id, &mut request).await?;
            }

            // The summary changes only when the history is compacted, so it sits *after* the persona
            // and skill catalog (which change less often still) and just before the history it stands in
            // for.
            if let Some(summary) = summary {
                let position = request
                    .messages
                    .iter()
                    .take_while(|message| message.role == Role::System)
                    .count();
                request
                    .messages
                    .insert(position, ChatMessage::system(summary_block(&summary)));
            }
        }

        for message in &mut request.messages {
            message.separate_reasoning();
        }
        // Apply policy after hooks but before validation, for ordinary turns and compaction.
        // Even duplicate hook-added schemas are irrelevant when this request disables tools.
        if !self.config.tool_calling || tools.is_none() {
            request.tools.clear();
        }
        ensure_unique_tool_names(request.tools.iter().map(|tool| tool.name.as_str()))?;
        normalize_request(&mut request)?;
        Ok(request)
    }

    /// Summarizes and folds away a session's history.
    ///
    /// See [`crate::compaction`] for why the summarization request is the conversation's own request
    /// plus one appended instruction.
    async fn compact(
        &self,
        session_id: &str,
        tools: Option<&[ToolDefinition]>,
        persona: Option<&Persona>,
        prepared: Option<PreparedCompaction>,
    ) -> Result<bool, AgentError> {
        // Both managed callers acquire their session writer before entering here. Embedded
        // callers share weakly retained writers so old session ids do not accumulate mutexes.
        let _guard = match &self.standalone_writers {
            Some(writers) => Some(writers.writer(session_id).lock_owned().await),
            None => None,
        };

        let mut snapshot = self.memory.snapshot(session_id).await?;
        let prepared_prefix = if let Some(prepared) = prepared {
            if snapshot.summary != prepared.snapshot.summary
                || !snapshot.messages.starts_with(&prepared.snapshot.messages)
            {
                // A reset or another compaction replaced the captured history. Its prefix must
                // never be used to summarize unrelated messages; dropping this task allows retry.
                return Ok(false);
            }
            // A later turn may have appended a suffix while this task waited. Summarize only the
            // captured prefix; compact_history preserves that suffix after the covered boundary.
            snapshot = prepared.snapshot;
            Some(prepared.prefix)
        } else {
            None
        };
        let min_messages = self
            .config
            .compaction
            .unwrap_or_default()
            .min_messages
            .max(1);
        if snapshot.messages.len() < min_messages || !ends_cleanly(&snapshot.messages) {
            return Ok(false);
        }
        let covered = snapshot.messages.len();
        let system_prompt = if prepared_prefix.is_none() {
            self.prepare_system_prompt(session_id, persona).await?
        } else {
            String::new()
        };

        let mut request = self
            .request_from(
                session_id,
                snapshot,
                tools,
                &system_prompt,
                prepared_prefix.as_deref(),
            )
            .await?;
        request
            .messages
            .push(ChatMessage::user(COMPACTION_INSTRUCTION));

        let mut response = self.provider.chat(&request).await?;
        for hook in &self.hooks {
            hook.on_llm_response(session_id, &mut response).await?;
        }

        // Compatible models may emit tool calls as text. Apply the normal turn's decoding
        // before accepting a summary, or the call markup would replace the entire history.
        response.separate_reasoning();
        normalize_textual_tool_calls(&mut response);

        // Partial or refused output cannot replace durable history, even when it contains text.
        // Compatible providers may omit the reason. Any explicit reason must confirm completion;
        // accepting an unknown pause/failure status would permanently discard unsummarized facts.
        if let Some(reason) = response.finish_reason.as_deref()
            && !matches!(reason, "stop" | "end_turn" | "stop_sequence" | "completed")
        {
            return Err(AgentError::Compaction(format!(
                "the model returned an incomplete summary (finish reason: {reason})"
            )));
        }

        // A model that ignores the instruction and asks for a tool, or answers with nothing, has
        // not produced a summary. Folding the history away on that would lose the conversation.
        let summary = response
            .content
            .as_deref()
            .map(crate::gateway::strip_reasoning_tags)
            .map(str::trim)
            .filter(|text| !text.is_empty() && !text.starts_with("<think>"));
        let Some(summary) = summary.filter(|_| response.tool_calls.is_empty()) else {
            return Err(AgentError::Compaction(
                "the model returned no summary (empty answer or a tool call)".to_string(),
            ));
        };

        self.memory
            .compact_history(session_id, covered, summary.to_string())
            .await?;

        tracing::info!(
            session_id = %session_id,
            compacted_messages = covered,
            summary_chars = summary.chars().count(),
            "Conversation compacted into a summary"
        );
        Ok(true)
    }

    /// Schedules a background compaction when a finished turn left the context large enough.
    ///
    /// Runs right after a reply is produced because that is when the provider's cache holds exactly
    /// the prefix the summarization request will re-send, and in the background so the user never
    /// waits for it. The size comes from the provider's own token count when it reports one (it
    /// includes the system block and tools) and from an estimate otherwise; the reply is added
    /// because it becomes part of the next request, including reasoning replayed with tools.
    async fn schedule_compaction(
        &self,
        session_id: &str,
        request: &ChatRequest,
        usage: Option<&TokenUsage>,
        reply: &ChatMessage,
    ) {
        let Some(policy) = self.config.compaction else {
            return;
        };

        let prompt_tokens = usage
            .map(|usage| usage.prompt_tokens as usize)
            .filter(|tokens| *tokens > 0)
            .unwrap_or_else(|| crate::token::estimate_request_tokens(request));
        let reply_tokens = usage
            .map(|usage| usage.completion_tokens as usize)
            .filter(|tokens| *tokens > 0)
            .unwrap_or_else(|| crate::token::estimate_message_tokens(reply));

        if !policy.is_exceeded(prompt_tokens + reply_tokens, self.config.context_length) {
            return;
        }

        // One compaction per session at a time; a second scheduled meanwhile would only summarize
        // what the first is already summarizing.
        if self.compacting.contains(session_id) {
            return;
        }

        // Capture only when compaction is needed, before this turn releases its writer. The
        // background task verifies this exact snapshot rather than guessing from message counts.
        let snapshot = match self.memory.snapshot(session_id).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(session_id, %error, "Could not prepare conversation compaction");
                return;
            }
        };
        if snapshot.messages.len() < policy.min_messages.max(1) || !ends_cleanly(&snapshot.messages)
        {
            return;
        }
        // Publish only after the last await before spawning. Cancelling the turn while reading
        // memory must not leave a permanent in-flight marker with no task that can clear it.
        if !self.compacting.insert(session_id.to_string()) {
            return;
        }
        let prepared = PreparedCompaction {
            snapshot,
            prefix: request
                .messages
                .iter()
                .take_while(|message| message.role == Role::System)
                .cloned()
                .collect(),
        };
        let agent = self.clone();
        let session_id = session_id.to_string();
        let tools = request.tools.clone();
        tokio::spawn(async move {
            // A queued compaction must never outlive a reset and restore its old summary.
            let _writing = match agent.session_manager.as_ref() {
                Some(sessions) => Some(sessions.write(&session_id).await),
                None => None,
            };
            if let Err(err) = agent
                .compact(&session_id, Some(&tools), None, Some(prepared))
                .await
            {
                tracing::warn!(
                    session_id = %session_id,
                    error = %err,
                    "Conversation compaction failed; the history is left as it was"
                );
            }
            agent.compacting.remove(&session_id);
        });
    }

    /// Answers one turn: records the user message, then runs the reasoning and tool loop.
    ///
    /// # Flow:
    /// 1. Lets hooks enrich the user message, then records it in memory (without its media);
    /// 2. Aggregates tools from native registered tools and plugin hosts;
    /// 3. Runs the reasoning loop until the model emits final text or the iteration ceiling is hit,
    ///    invoking hooks around requests, tool authorizations and completions;
    /// 4. Dispatches tool calls (in-process for native tools, gRPC IPC for plugin tools);
    /// 5. Stores the assistant reply in memory and returns [`AgentOutput`].
    ///
    /// The textual projection in [`ChatMessage::content`] is what token accounting and summaries
    /// use, and the only part of the message stored in history: the images go out with this
    /// turn's requests alone.
    async fn run_turn(
        &self,
        session_id: &str,
        message: ChatMessage,
        hosts: &[Arc<dyn ToolHost>],
        mut options: TurnOptions,
    ) -> Result<AgentOutput, AgentError> {
        options.persona = self.resolve_persona(session_id, options.persona)?;
        let system_prompt = self
            .prepare_system_prompt(session_id, options.persona.as_ref())
            .await?;
        let media = self.start_turn(session_id, message).await?;

        // From here on the turn is part of history. However it ends, the next turn has to find a
        // conversation the provider accepts, and one that does not ask the model to redo the work
        // that just failed.
        match self
            .answer(session_id, media, hosts, options, &system_prompt, None)
            .await
        {
            Ok(output) => Ok(output),
            Err(err) => Err(self.close_failed_turn(session_id, err).await),
        }
    }

    /// Enriches the message under the caller's task scope, then commits the turn once.
    async fn start_turn(
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
    async fn answer(
        &self,
        session_id: &str,
        media: Option<Vec<ContentPart>>,
        hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
        system_prompt: &str,
        stream: Option<&tokio::sync::mpsc::Sender<Result<ChatChunk, AgentError>>>,
    ) -> Result<AgentOutput, AgentError> {
        // Model capability and per-turn policy share one execution boundary. The same decision
        // controls advertisement and dispatch, including calls introduced by middleware.
        let tools_enabled = self.config.tool_calling && !options.without_tools;
        let tools = if tools_enabled {
            self.collect_tools(hosts)?
        } else {
            TurnTools::default()
        };
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
            normalize_textual_tool_calls(&mut response);
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
    async fn close_failed_turn(&self, session_id: &str, err: AgentError) -> AgentError {
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
    async fn record_closing(
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
    async fn complete_round(
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
    async fn stream_turn(
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
        let system_prompt = self
            .prepare_system_prompt(session_id, options.persona.as_ref())
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

#[async_trait]
impl Agent for BuiltinAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn config(&self) -> &AgentConfig {
        &self.config
    }

    fn provider(&self) -> &Arc<dyn LlmProvider> {
        &self.provider
    }

    fn memory(&self) -> &Arc<dyn Memory> {
        &self.memory
    }

    fn session_manager(&self) -> Option<&Arc<crate::session::SessionManager>> {
        self.session_manager.as_ref()
    }

    fn persona_registry(&self) -> Option<&Arc<crate::prompt::PersonaRegistry>> {
        self.persona_registry.as_ref()
    }

    async fn run_message_with(
        &self,
        session_id: &str,
        message: ChatMessage,
        hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
    ) -> Result<AgentOutput, AgentError> {
        let _writing = self.turn_writer(session_id)?;
        self.run_turn(session_id, message, hosts, options).await
    }

    async fn run_stream(
        &self,
        session_id: &str,
        user_input: &str,
        hosts: &[Arc<dyn ToolHost>],
    ) -> Result<ChatChunkStream<AgentError>, AgentError> {
        self.stream_turn(session_id, user_input, hosts).await
    }

    /// Compacts a session's history into a summary now, whatever its size.
    ///
    /// Returns `Ok(true)` when the history was replaced by a summary, and `Ok(false)` when there was
    /// nothing to compact (too short, or not at a clean stopping point — see [`ends_cleanly`]). The
    /// tools of `hosts` are part of the summarization request so its prefix matches the
    /// conversation's own requests and the provider's cache is reused.
    async fn compact_session_with(
        &self,
        session_id: &str,
        hosts: &[Arc<dyn ToolHost>],
        options: TurnOptions,
    ) -> Result<bool, AgentError> {
        let _writing = self
            .session_manager
            .as_ref()
            .map(|sessions| sessions.agent_write(session_id))
            .transpose()?;
        let persona = self.resolve_persona(session_id, options.persona)?;
        let tools_enabled = self.config.tool_calling && !options.without_tools;
        let tools = if tools_enabled {
            self.collect_tools(hosts)?
        } else {
            TurnTools::default()
        };
        self.compact(
            session_id,
            tools_enabled.then_some(tools.definitions.as_slice()),
            persona.as_ref(),
            None,
        )
        .await
    }
}

/// Removes tool-produced filesystem paths from a tool result before it enters memory.
///
/// A tool that generated a file reports the attachment (which the pipeline delivers to the platform)
/// and, very often, the path in its textual result. The path is node-internal: leaving it in the
/// transcript leaks an absolute location to the model and to anyone reading the console, so it is
/// replaced by a marker — the file itself is still sent.
fn redact_tool_paths(mut text: String, paths: &[String]) -> String {
    for path in paths {
        let trimmed = path.trim();
        if !trimmed.is_empty() && text.contains(trimmed) {
            text = text.replace(trimmed, "[attachment]");
        }
    }
    text
}

/// Joins the reasoning collected across one turn's rounds, or `None` when there was none.
fn joined_reasoning(rounds: &[String]) -> Option<String> {
    (!rounds.is_empty()).then(|| rounds.join("\n\n"))
}

/// Puts the turn's media back on its user message for one request of the turn.
///
/// The turn's message is the last user message: everything after it is this turn's own tool loop,
/// which holds only assistant and tool messages. The next turn's requests no longer carry it, so
/// the prefix they share with this one ends just before this message — a one-time cache miss of
/// that message instead of re-sending every picture of the conversation with each later request.
fn attach_turn_media(request: &mut ChatRequest, media: Option<&Vec<ContentPart>>) {
    let Some(media) = media else {
        return;
    };
    if let Some(message) = request
        .messages
        .iter_mut()
        .rev()
        .find(|message| message.role == Role::User)
    {
        message.parts = Some(media.clone());
    }
}

/// Ids of the tool calls in the last tool-calling round of `history` that have no result yet.
fn unanswered_tool_calls(history: &[ChatMessage]) -> Vec<String> {
    let Some(round) = history.iter().rposition(|message| {
        message.role == Role::Assistant
            && message
                .tool_calls
                .as_ref()
                .is_some_and(|calls| !calls.is_empty())
    }) else {
        return Vec::new();
    };
    let answered: std::collections::HashSet<&str> = history[round + 1..]
        .iter()
        .filter(|message| message.role == Role::Tool)
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect();
    history[round]
        .tool_calls
        .iter()
        .flatten()
        .filter(|call| !answered.contains(call.id.as_str()))
        .map(|call| call.id.clone())
        .collect()
}

/// The note that closes an unanswered turn in history, written for the model that reads it on
/// the next turn: what happened, that nothing retried it, and that the user decides about retrying.
///
/// It names the kind of failure rather than quoting the error, whose provider body can be long
/// and would sit in every later request of the session.
fn closing_note(err: &AgentError) -> String {
    let what = match err {
        AgentError::Busy(_) => "failed because the session is busy".to_string(),
        AgentError::Stopped => "was stopped by the user before it finished".to_string(),
        AgentError::Gateway(GatewayError::Http(http)) if http.is_timeout() => {
            "failed: the model request timed out".to_string()
        }
        AgentError::Gateway(GatewayError::Http(_)) => {
            "failed: the model service could not be reached".to_string()
        }
        AgentError::Gateway(GatewayError::ApiStatus { status, .. }) => {
            format!("failed: the model service answered HTTP {status}")
        }
        AgentError::Gateway(GatewayError::Json(_) | GatewayError::InvalidResponse(_)) => {
            "failed: the model's answer could not be read".to_string()
        }
        AgentError::Rpc(_) => "failed: a plugin tool call failed".to_string(),
        AgentError::ToolFailed(_) => "failed: a tool execution failed".to_string(),
        AgentError::ToolNotFound(name) => format!("failed: the tool '{name}' does not exist"),
        AgentError::Memory(_) => "failed: conversation storage failed".to_string(),
        AgentError::InvalidRequest(_) => {
            "failed: model request configuration is invalid".to_string()
        }
        AgentError::Compaction(_) => "failed: conversation compaction failed".to_string(),
    };
    format!(
        "[This turn {what}. It was not retried. Answer the next message on its own, and redo this \
         request only if the user asks for it again.]"
    )
}

/// Recovers tool calls from a completion whose model emitted markup instead of a structured array.
///
/// Some endpoints ignore the `tools` request field and answer with `<tool_call>...` text. Turning
/// that text back into real calls is what makes the reasoning loop execute the tool and continue;
/// without it the markup is returned to the user as the assistant's answer.
fn normalize_textual_tool_calls(response: &mut ChatResponse) {
    if !response.tool_calls.is_empty() {
        return;
    }
    let Some(content) = response.content.as_deref() else {
        return;
    };

    let (recovered, cleaned) = crate::tool_call_text::extract_textual_tool_calls(content);
    if recovered.is_empty() {
        return;
    }

    tracing::info!(
        tool_calls = recovered.len(),
        tools = ?recovered.iter().map(|call| call.name.as_str()).collect::<Vec<_>>(),
        "Recovered tool calls a model emitted as text markup"
    );

    response.tool_calls = recovered;
    response.content = if cleaned.trim().is_empty() {
        None
    } else {
        Some(cleaned)
    };
}

/// Fluent builder for constructing customizable [`BuiltinAgent`] instances.
pub struct AgentBuilder {
    name: String,
    system_prompt: Option<String>,
    provider: Arc<dyn LlmProvider>,
    memory: Option<Arc<dyn Memory>>,
    session_manager: Option<Arc<crate::session::SessionManager>>,
    persona_registry: Option<Arc<crate::prompt::PersonaRegistry>>,
    tools: Vec<Arc<dyn AgentTool>>,
    hooks: Vec<Arc<dyn AgentHook>>,
    config: AgentConfig,
}

impl AgentBuilder {
    /// Initiates a new builder with the agent name and backend model provider.
    pub fn new(name: impl Into<String>, provider: Arc<dyn LlmProvider>) -> Self {
        Self {
            name: name.into(),
            system_prompt: None,
            provider,
            memory: None,
            session_manager: None,
            persona_registry: None,
            tools: Vec::new(),
            hooks: Vec::new(),
            config: AgentConfig::default(),
        }
    }

    /// Sets the base persona / system prompt for the agent.
    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Injects a custom memory backend (e.g. SQLite, Redis, or custom plugin memory).
    pub fn memory(mut self, memory: Arc<dyn Memory>) -> Self {
        self.memory = Some(memory);
        self
    }

    /// Injects a session manager for metadata, multi-scope keys, and turn lifecycle tracking.
    pub fn session_manager(mut self, manager: Arc<crate::session::SessionManager>) -> Self {
        self.session_manager = Some(manager);
        self
    }

    /// Injects a persona registry for dynamic persona resolution.
    pub fn persona_registry(mut self, registry: Arc<crate::prompt::PersonaRegistry>) -> Self {
        self.persona_registry = Some(registry);
        self
    }

    /// Registers a native in-process tool on this agent.
    pub fn tool(mut self, tool: impl AgentTool + 'static) -> Self {
        self.tools.push(Arc::new(tool));
        self
    }

    /// Registers a native in-process tool wrapped in an [`Arc`].
    pub fn tool_arc(mut self, tool: Arc<dyn AgentTool>) -> Self {
        self.tools.push(tool);
        self
    }

    /// Registers a lifecycle interception hook on this agent.
    pub fn hook(mut self, hook: impl AgentHook + 'static) -> Self {
        self.hooks.push(Arc::new(hook));
        self
    }

    /// Registers a lifecycle hook wrapped in an [`Arc`].
    pub fn hook_arc(mut self, hook: Arc<dyn AgentHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Sets default model tag.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.config.default_model = model.into();
        self
    }

    /// Names the provider endpoint that serves the default model.
    pub fn provider(mut self, provider: Option<String>) -> Self {
        self.config.provider = provider;
        self
    }

    /// Records the default model's context window, when known.
    pub fn context_length(mut self, context_length: Option<u32>) -> Self {
        self.config.context_length = context_length;
        self
    }

    /// Sets maximum reasoning iterations (default 5).
    pub fn max_iterations(mut self, max: usize) -> Self {
        self.config.max_iterations = max;
        self
    }

    /// Allows tool schemas and execution only when the selected model supports tool calling.
    pub fn tool_calling(mut self, enabled: bool) -> Self {
        self.config.tool_calling = enabled;
        self
    }

    /// Sets sampling temperature.
    pub fn temperature(mut self, temp: f32) -> Self {
        self.config.temperature = Some(temp);
        self
    }

    /// Sets max tokens.
    pub fn max_tokens(mut self, tokens: u32) -> Self {
        self.config.max_tokens = Some(tokens);
        self
    }

    /// Configures whether to stop immediately if a tool fails.
    pub fn stop_on_tool_failure(mut self, stop: bool) -> Self {
        self.config.stop_on_tool_failure = stop;
        self
    }

    /// Sets when long conversations are compacted into a summary; `None` never compacts.
    pub fn compaction(mut self, policy: Option<CompactionPolicy>) -> Self {
        self.config.compaction = policy;
        self
    }

    /// Builds the configured [`BuiltinAgent`].
    pub fn build(self) -> BuiltinAgent {
        let memory = self
            .memory
            .or_else(|| self.session_manager.as_ref().map(|sm| sm.memory().clone()))
            .unwrap_or_else(|| Arc::new(InMemory::new()));

        let standalone_writers = self
            .session_manager
            .is_none()
            .then(|| Arc::new(SessionWriters::default()));

        BuiltinAgent {
            name: self.name,
            system_prompt: self.system_prompt,
            provider: self.provider,
            memory,
            session_manager: self.session_manager,
            persona_registry: self.persona_registry,
            tools: self.tools,
            hooks: self.hooks,
            config: self.config,
            standalone_writers,
            compacting: Arc::new(DashSet::new()),
        }
    }
}
