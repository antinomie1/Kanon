//! Stable request prefixes, persona resolution and history compaction.

use super::*;

impl BuiltinAgent {
    /// Resolves one immutable persona before recording a turn or beginning manual compaction.
    ///
    /// Instance inheritance overrides an explicit session choice without mutating it. Unknown
    /// bindings fail before history changes; tool rounds never look up a different persona later.
    pub(super) fn resolve_persona(
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
    pub(super) async fn prepare_system_prompt(
        &self,
        session_id: &str,
        persona: Option<&Persona>,
        tools: &[ToolDefinition],
        instructions: Option<&str>,
    ) -> Result<String, AgentError> {
        let mut parts = Vec::new();
        if let Some(persona) = persona {
            parts.push(ChatMessage::system(persona.prompt.clone()));
        }
        if let Some(prompt) = &self.system_prompt {
            parts.push(ChatMessage::system(prompt.clone()));
        }
        if let Some(instructions) = instructions {
            parts.push(ChatMessage::system(instructions));
        }
        let mut prompt = crate::layout::system_text(&parts);
        for hook in &self.hooks {
            hook.on_system_prompt(session_id, &mut prompt, tools)
                .await?;
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
    pub(super) async fn build_request(
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
    pub(super) async fn request_from(
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
    pub(super) async fn compact(
        &self,
        session_id: &str,
        tools: Option<&[ToolDefinition]>,
        persona: Option<&Persona>,
        prepared: Option<PreparedCompaction>,
        instructions: Option<&str>,
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
            self.prepare_system_prompt(session_id, persona, tools.unwrap_or_default(), instructions)
                .await?
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
        normalize_textual_tool_calls(&mut response)?;

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
    pub(super) async fn schedule_compaction(
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
                .compact(&session_id, Some(&tools), None, Some(prepared), None)
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
}
