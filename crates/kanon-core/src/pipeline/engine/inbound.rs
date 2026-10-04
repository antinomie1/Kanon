//! Inbound filtering, command dispatch and agent routing.

use super::*;

impl PipelineEngine {
    /// Processes a single inbound event through the PreFilter chain and command dispatcher.
    pub async fn process_event(&self, event: PipelineEventRequest) -> PipelineResult {
        let event_id = event.event_id.clone();
        let platform = event.platform.clone();
        let hosts = self.supervisor.get_all_hosts().await;

        // Phase 0: Instance gate.
        //
        // Adapters only declare *where* messages come from; an instance decides whether a bot is
        // running there at all. With no enabled instance claiming the platform there is nothing
        // to answer as, so the event is dropped here — before pre-filters, commands and the LLM.
        let instance = match &self.instances {
            Some(registry) => match registry.resolve_by_platform(&platform).await {
                Ok(Some(instance)) => Some(instance),
                Ok(None) => {
                    tracing::warn!(
                        platform = %platform,
                        event_id = %event_id,
                        "No enabled bot instance claims this platform; dropping inbound event"
                    );
                    return PipelineResult::NoInstance { platform };
                }
                Err(err) => {
                    // Ambiguous ownership must never be resolved by guessing.
                    tracing::error!(
                        platform = %platform,
                        event_id = %event_id,
                        error = %err,
                        "Instance routing is ambiguous; dropping inbound event"
                    );
                    return PipelineResult::NoInstance { platform };
                }
            },
            None => None,
        };

        // Phase 0b: Per-instance plugin policy.
        //
        // Filtering here (rather than inside each later phase) means a plugin this instance
        // disabled cannot pre-filter, answer commands, offer tools or observe events — one
        // decision covers the whole pipeline. It runs before notices so a notice reaches only
        // the plugins this instance runs.
        let hosts = self.instance_hosts(instance.as_ref(), hosts).await;

        // Phase 0a: Notices.
        //
        // A join, poke or recall is not a message. Only the node's event policy decides whether the
        // bot reacts at all; a notice it reacts to then skips commands and the reply policy (the
        // operator explicitly asked for the reaction) and reaches the model as a one-line event.
        let notice = NoticeKind::from_metadata(event.metadata.as_ref());
        // Bash identity is taken from the adapter's event before plugin pre-filters can rewrite
        // it. A notice is not a message: its "sender" is a joining member or a poker, who never
        // asked for anything, so a notice turn has no caller.
        let bash_sender = (notice.is_none() && !event.sender_id.trim().is_empty())
            .then(|| format!("{platform}:{}", event.sender_id));
        if let Some(kind) = notice {
            // Subscribers observe every notice, whether or not the bot itself reacts to it.
            hooks::emit_event(&hosts, EventKind::Notice, Detail::Notice(event.clone()));
            let policy = self
                .event_policy
                .as_ref()
                .map(|store| store.get())
                .unwrap_or_default();
            let outcome = if kind == NoticeKind::Recall {
                let target = metadata_str(event.metadata.as_ref(), META_NOTICE_TARGET);
                let actor = metadata_str(event.metadata.as_ref(), META_NOTICE_ACTOR);
                match target {
                    Some(target) if policy.note_recalls => {
                        if self.recalls.note_recall(target, actor) {
                            "noted for the conversation's next turn".to_string()
                        } else {
                            "the model never saw the recalled message".to_string()
                        }
                    }
                    Some(_) => "recall notes are disabled by the event policy".to_string(),
                    None => "the recall names no message".to_string(),
                }
            } else if matches!(kind, NoticeKind::FriendRequest | NoticeKind::GroupInvite) {
                if !policy.accepts(kind) {
                    "left for a human: the event policy does not accept it".to_string()
                } else if let Some(adapter) = self.supervisor.adapters().get(&platform).await {
                    // Spawned: accepting is platform I/O and must not hold up the pipeline.
                    let request = event.clone();
                    tokio::spawn(async move {
                        match adapter.accept_request(&request).await {
                            Ok(()) => tracing::info!(
                                event_id = %request.event_id,
                                "Request accepted automatically by the event policy"
                            ),
                            Err(err) => tracing::warn!(
                                event_id = %request.event_id,
                                error = %err,
                                "Request could not be accepted"
                            ),
                        }
                    });
                    "accepted automatically".to_string()
                } else {
                    "no built-in adapter serves the platform to accept it".to_string()
                }
            } else if !policy.answers(kind) {
                "disabled by the event policy".to_string()
            } else {
                String::new()
            };
            if !outcome.is_empty() {
                return PipelineResult::Notice {
                    kind: kind.as_str(),
                    outcome,
                };
            }
        }

        // Phase 1: PreFilter Interception Chain
        self.observe(PipelineStage::PreFilterStarted {
            event_id: event_id.clone(),
            host_count: hosts.len(),
        });
        let filtered_event = match PreFilterChain::execute_with_observer(
            event,
            &hosts,
            self.observer.as_ref(),
        )
        .await
        {
            PreFilterOutcome::Blocked {
                host_id,
                reply_messages,
            } => {
                tracing::info!(
                    host_id = %host_id,
                    "Event blocked by PreFilter chain; halting pipeline execution"
                );
                self.observe(PipelineStage::PreFilterBlocked {
                    event_id,
                    host_id: host_id.clone(),
                });
                return PipelineResult::Blocked {
                    host_id,
                    replies: reply_messages,
                };
            }
            PreFilterOutcome::Passed(evt) => evt,
        };
        self.observe(PipelineStage::PreFilterPassed {
            event_id: event_id.clone(),
        });

        // Phase 2: Command Router matching
        let text_candidate = message_text(&filtered_event);

        // Phase 2a: Built-in commands, resolved by the core itself.
        //
        // `/new` rotates the session of the conversation that issued it and `/model` lists or
        // switches the instance's model; `/help` and `/info` report the command catalog and the
        // node's runtime facts. All are matched before plugin commands (so a plugin can never
        // shadow them) and long before the LLM, which must never receive them as conversation text.
        //
        // Group platforms render a mention as leading text (`@bot /model`), so mentions are
        // stripped before parsing; otherwise a command typed in a group would never be recognised.
        // A notice carries no user text, so it can never be a command.
        let command_text = if notice.is_some() {
            ""
        } else {
            strip_leading_mentions(&text_candidate)
        };
        // The policy that decides who may run commands and fire triggers. Without a policy store
        // (an embedded pipeline) everything stays open, which is the pre-policy behaviour; the
        // node always installs one. An instance with its own command policy replaces the node's,
        // administrators included.
        let command_policy = self.command_policy.as_ref().map(|store| {
            instance.as_ref().map_or_else(
                || store.get(),
                |instance| instance.effective_command_policy(store.get()),
            )
        });

        // Phase 2a: Conversation captures.
        //
        // A plugin that asked a question gets the sender's next message before anything else may
        // interpret it — even text that looks like a command is the answer it is waiting for.
        // The capture is consumed here either way, so a plugin that has been disabled or whose
        // host is gone cannot keep swallowing the sender's messages.
        //
        // Phases 2a–2b run in a labelled block: a handler that hands the message on to the model
        // (`pass_to_model`) leaves it with the event the model should read, skipping the
        // remaining handlers, while every other outcome returns from the pipeline directly.
        let filtered_event = 'handlers: {
            if notice.is_none()
                && let Some(capture) = self.captures.take(&filtered_event)
            {
                match hosts.iter().find(|host| host.host_id == capture.host_id) {
                    Some(host) => {
                        tracing::debug!(
                            command = %capture.command,
                            plugin_id = %capture.plugin_id,
                            host_id = %host.host_id,
                            "Routing captured message to the waiting plugin"
                        );
                        self.observe(PipelineStage::CommandMatched {
                            event_id: event_id.clone(),
                            command: capture.command.clone(),
                            plugin_id: capture.plugin_id.clone(),
                            host_id: host.host_id.clone(),
                        });
                        let outcome = CommandRouter::dispatch_continuation(
                            host,
                            &capture,
                            command_text,
                            filtered_event.clone(),
                        )
                        .await;
                        match self
                            .command_result(
                                &hosts,
                                capture.command,
                                &capture.plugin_id,
                                host,
                                &filtered_event,
                                outcome,
                            )
                            .await
                        {
                            CommandFlow::Finished(result) => return result,
                            CommandFlow::PassToModel(event) => break 'handlers event,
                        }
                    }
                    None => tracing::warn!(
                        plugin_id = %capture.plugin_id,
                        host_id = %capture.host_id,
                        "Captured conversation's plugin is no longer active here; processing the message normally"
                    ),
                }
            }

            if let Some(parsed) = CommandRouter::parse_command(command_text) {
                let name = parsed.name.clone();
                // The conversation commands, `/model` and `/stop` act on an instance, so without
                // one they are ordinary names a plugin may claim; `/help` and `/info` are always
                // the core's.
                let builtin = ((CONVERSATION_COMMANDS
                    .iter()
                    .any(|command| name.eq_ignore_ascii_case(command))
                    || name.eq_ignore_ascii_case(MODEL_COMMAND)
                    || name.eq_ignore_ascii_case(STOP_COMMAND))
                    && instance.is_some())
                    || name.eq_ignore_ascii_case(HELP_COMMAND)
                    || name.eq_ignore_ascii_case(INFO_COMMAND);
                let target = if builtin {
                    None
                } else {
                    CommandRouter::resolve(&name, &hosts, &filtered_event)
                };

                // Phase 2-: Command permissions, checked once for built-in and plugin commands alike.
                // A plugin command is checked under its canonical name, so an alias can never bypass
                // a restriction, and with the access level its plugin declared as the default.
                let (policy_name, default_access) = match &target {
                    Some(target) => (target.name().to_string(), target.access()),
                    // Stopping cuts off answers in every chat of the instance, not only the
                    // sender's own.
                    None if name.eq_ignore_ascii_case(STOP_COMMAND) => {
                        (STOP_COMMAND.to_string(), CommandAccess::Admins)
                    }
                    // In a group that shares one session, switching or deleting the conversation
                    // does it for every member, so only administrators may by default.
                    None if instance.is_some()
                        && (name.eq_ignore_ascii_case(SWITCH_SESSION_COMMAND)
                            || name.eq_ignore_ascii_case(DELETE_SESSION_COMMAND)) =>
                    {
                        (name.to_ascii_lowercase(), CommandAccess::AdminsInGroups)
                    }
                    None => (name.clone(), CommandAccess::Everyone),
                };
                if let Some(policy) = command_policy.as_ref()
                    && !policy.allows_with_default(&policy_name, default_access, &filtered_event)
                {
                    // The sender's own ID is part of the answer: it is exactly what an operator adds
                    // to the administrator list to grant access.
                    let reply = format!(
                        "/{name} 仅限管理员使用（你的 ID：{}:{}）",
                        filtered_event.platform, filtered_event.sender_id
                    );
                    return PipelineResult::CommandDenied {
                        command: policy_name,
                        replies: vec![text_reply(reply)],
                    };
                }

                if builtin {
                    if name.eq_ignore_ascii_case(HELP_COMMAND) {
                        return self.handle_help_command(&hosts);
                    }
                    if name.eq_ignore_ascii_case(INFO_COMMAND) {
                        return self.handle_info_command(instance.as_ref(), &filtered_event);
                    }
                    if let Some(instance) = instance.as_ref() {
                        if name.eq_ignore_ascii_case(NEW_SESSION_COMMAND) {
                            return self.handle_new_session(&filtered_event, instance).await;
                        }
                        if let Some(command) = CONVERSATION_COMMANDS
                            .iter()
                            .find(|command| name.eq_ignore_ascii_case(command))
                        {
                            return self
                                .handle_conversation_command(
                                    command,
                                    &filtered_event,
                                    instance,
                                    &parsed.args,
                                )
                                .await;
                        }
                        if name.eq_ignore_ascii_case(STOP_COMMAND) {
                            return self.handle_stop_command(instance);
                        }
                        return self
                            .handle_model_command(instance, &filtered_event, &parsed.args)
                            .await;
                    }
                }

                let Some(target) = target else {
                    tracing::debug!(
                        command = %name,
                        "Slash command detected but no matching plugin was registered"
                    );
                    self.observe(PipelineStage::CommandNotFound {
                        event_id,
                        command: name.clone(),
                    });
                    return PipelineResult::CommandNotFound { command: name };
                };

                let command = target.name().to_string();
                tracing::debug!(
                    command = %command,
                    typed = %name,
                    plugin_id = %target.plugin_id,
                    host_id = %target.host.host_id,
                    "Routing slash command to target host"
                );
                self.observe(PipelineStage::CommandMatched {
                    event_id: event_id.clone(),
                    command: command.clone(),
                    plugin_id: target.plugin_id.clone(),
                    host_id: target.host.host_id.clone(),
                });
                let outcome =
                    CommandRouter::dispatch(&target, parsed, filtered_event.clone()).await;
                match self
                    .command_result(
                        &hosts,
                        command,
                        &target.plugin_id,
                        &target.host,
                        &filtered_event,
                        outcome,
                    )
                    .await
                {
                    CommandFlow::Finished(result) => return result,
                    CommandFlow::PassToModel(event) => break 'handlers event,
                }
            }

            // Phase 2b: Plugin triggers.
            //
            // A trigger is a plugin's claim on a plain message by pattern. It runs where a command
            // would — after pre-filters, before the reply policy and the model — because a plugin that
            // asked for "messages matching X" wants them whether or not the bot was mentioned.
            if notice.is_none()
                && !command_text.trim().is_empty()
                && let Some(target) =
                    self.triggers
                        .resolve(command_text, &filtered_event, &hosts, |meta| {
                            command_policy.as_ref().is_none_or(|policy| {
                                policy.allows_with_default(
                                    &meta.name,
                                    CommandAccess::from_proto(meta.access()),
                                    &filtered_event,
                                )
                            })
                        })
            {
                let trigger = target.meta.name.clone();
                tracing::debug!(
                    trigger = %trigger,
                    plugin_id = %target.plugin_id,
                    host_id = %target.host.host_id,
                    "Routing message to the plugin trigger it matched"
                );
                self.observe(PipelineStage::CommandMatched {
                    event_id: event_id.clone(),
                    command: trigger.clone(),
                    plugin_id: target.plugin_id.clone(),
                    host_id: target.host.host_id.clone(),
                });
                let outcome = CommandRouter::dispatch_trigger(
                    &target,
                    command_text.trim(),
                    filtered_event.clone(),
                )
                .await;
                match self
                    .command_result(
                        &hosts,
                        trigger,
                        &target.plugin_id,
                        &target.host,
                        &filtered_event,
                        outcome,
                    )
                    .await
                {
                    CommandFlow::Finished(result) => return result,
                    CommandFlow::PassToModel(event) => break 'handlers event,
                }
            }

            filtered_event
        };

        // Phase 2c: Reply policy gate.
        //
        // Evaluated after commands and before the model: built-in and plugin slash commands always
        // answer, while an unaddressed group message never reaches the LLM when the instance asked
        // to be mentioned first. Pre-filters have already run, so a plugin still observes every
        // inbound event — this gate only decides whether the *bot* answers.
        let kind = ConversationKind::from_metadata(filtered_event.metadata.as_ref());
        let node_reply_policy = self
            .reply_policy
            .as_ref()
            .map(|store| store.get())
            .unwrap_or_default();
        let reply_policy = instance.as_ref().map_or(node_reply_policy, |instance| {
            instance.effective_reply_policy(node_reply_policy)
        });
        // Quoting only makes sense for a message in a shared conversation; a notice has none.
        let quote_reply =
            reply_policy.quote_message && kind.is_policy_governed() && notice.is_none();

        // Group context: a shared session labels every message with its speaker, and an observing
        // instance records every group message — answered or not — for its next turn.
        let shared = shares_session(instance.as_ref(), &filtered_event);
        let observing = instance
            .as_ref()
            .is_some_and(|instance| instance.observe_group)
            && kind.is_policy_governed()
            && notice.is_none();
        let group_key = format!("{platform}\u{1f}{}", filtered_event.channel_id);
        let context_policy = instance.as_ref().map_or_else(
            || {
                self.context_policy
                    .as_ref()
                    .map(|store| store.get())
                    .unwrap_or_default()
            },
            |instance| {
                instance.effective_context_policy(
                    self.context_policy
                        .as_ref()
                        .map(|store| store.get())
                        .unwrap_or_default(),
                )
            },
        );
        let speaker = crate::pipeline::identity::speaker_label(
            &filtered_event,
            context_policy.include_sender_id,
        );

        // Resolve the runtime before simulation or model lookup. A selected external backend
        // owns the entire conversation and must never reach the builtin loop as a fallback.
        let resolved_agent = match &self.agent_factory {
            Some(factory) => match factory.conversation_backend(
                instance.as_ref().and_then(|i| i.agent.as_deref()),
                instance.as_ref().and_then(|i| i.model.as_deref()),
            ) {
                Ok(agent) => agent,
                Err(error) => {
                    return PipelineResult::LlmFailed {
                        error,
                        replies: vec![text_reply("Agent 后端未就绪，请检查实例配置。")],
                    };
                }
            },
            None if instance
                .as_ref()
                .and_then(|instance| instance.agent.as_deref())
                .is_some_and(|agent| agent != kanon_llm::BUILTIN_AGENT) =>
            {
                return PipelineResult::LlmFailed {
                    error: "The selected external agent requires a configured factory".into(),
                    replies: vec![text_reply("Agent 后端未就绪，请检查实例配置。")],
                };
            }
            None => self
                .agent
                .current()
                .map(kanon_llm::ConversationBackend::Builtin),
        };

        #[cfg(feature = "dsh")]
        if instance.as_ref().is_some_and(|instance| {
            instance.conversation_mode == crate::simulation::ConversationMode::Simulation
        }) && matches!(
            &resolved_agent,
            Some(kanon_llm::ConversationBackend::Dsh(_))
        ) {
            return PipelineResult::LlmFailed {
                error: "DSH simulation requires the Kanon agent bridge".into(),
                replies: vec![text_reply("DSH 群聊仿真桥接尚未就绪。")],
            };
        }
        if let Some(instance) = instance.as_ref()
            && resolved_agent
                .as_ref()
                .is_none_or(|agent| agent.builtin().is_some())
            && instance.conversation_mode == crate::simulation::ConversationMode::Simulation
            && notice.is_none()
        {
            return self
                .process_simulation(
                    instance,
                    filtered_event,
                    &hosts,
                    reply_policy,
                    context_policy,
                )
                .await;
        }

        if let Some(instance) = instance.as_ref()
            && notice.is_none()
        {
            let mentioned = bot_mentioned(filtered_event.metadata.as_ref());
            let policy = reply_policy;
            let sample = reply_sample(&filtered_event.event_id);
            if !policy.should_reply(kind, mentioned, sample) {
                let reason = format!(
                    "reply policy '{}' suppressed a {} conversation (mentioned={mentioned})",
                    policy.describe(),
                    kind.as_str()
                );
                tracing::info!(
                    instance_id = %instance.id,
                    event_id = %filtered_event.event_id,
                    reason = %reason,
                    "Reply suppressed by the instance reply policy"
                );
                if observing {
                    self.group_log
                        .record(&group_key, &speaker, &filtered_event.raw_text);
                }
                return PipelineResult::ReplySuppressed {
                    instance_id: instance.id.clone(),
                    reason,
                };
            }
        }

        // Phase 3: Conversational message (unmatched by command router, routed to LLM if enabled)
        // The agent is resolved per event so a provider configured or cleared at runtime is
        // honoured immediately; an empty slot means "no conversational LLM" and passes through.
        let conversation = instance_conversation_key(&filtered_event, instance.as_ref());

        // The target model's catalog entry decides which modalities may be attached: an image only
        // when it accepts images, text only when it accepts text.
        let capabilities = match (
            self.agent_factory.as_ref(),
            resolved_agent.as_ref().and_then(|a| a.builtin()),
        ) {
            (Some(factory), Some(agent)) => {
                factory
                    .models()
                    .settings_for(&ModelRef::parse(&agent.config().model_ref()))
                    .capabilities
            }
            _ => ModelCapabilities::default(),
        };
        #[cfg(feature = "dsh")]
        let capabilities = if matches!(
            &resolved_agent,
            Some(kanon_llm::ConversationBackend::Dsh(_))
        ) {
            // Intake support belongs to DSH's model configuration, not the builtin catalog.
            ModelCapabilities {
                vision: true,
                ..ModelCapabilities::default()
            }
        } else {
            capabilities
        };
        // Recall notes waiting for this conversation ride on its next model turn, as leading text
        // of the current user message: runtime facts belong to the current turn, never to the
        // cached prefix. They are only taken when a model will actually read them.
        let ledger_key = format!("{platform}\u{1f}{conversation}");
        // Sessions are namespaced by the instance that owns the conversation, so two bots can
        // never share context. An unpartitioned pipeline keeps the legacy conversation key.
        let session_id = match instance.as_ref() {
            Some(instance) => {
                #[cfg(feature = "dsh")]
                if matches!(
                    &resolved_agent,
                    Some(kanon_llm::ConversationBackend::Dsh(_))
                ) {
                    instance.dsh_session_id_at(
                        &conversation,
                        instance.dsh_session_generation(&conversation),
                    )
                } else {
                    instance.conversation_session_id(&conversation)
                }
                #[cfg(not(feature = "dsh"))]
                instance.conversation_session_id(&conversation)
            }
            None => conversation.clone(),
        };
        let mut filtered_event = filtered_event;
        let answering = resolved_agent.is_some();
        let mut lead: Vec<String> = Vec::new();
        if answering {
            lead.extend(self.recalls.take_notes(&ledger_key));
            // Plugin context joins the current turn only, after the core's own notes and before
            // the speaker label, which must stay directly in front of the message it introduces.
            // Preparers see the message as the user sent it, before any of this leading text.
            if notice.is_none() {
                lead.extend(hooks::prepare_turn(&hosts, &filtered_event, &session_id).await);
            }
        }
        // Observed group lines this session has not seen come first, then the speaker of the
        // current message; the current message is recorded so other sessions see it later.
        let mut log_block = None;
        if observing {
            let unseen = if answering {
                self.group_log.unseen(&group_key, &ledger_key)
            } else {
                Vec::new()
            };
            let seq = self
                .group_log
                .record(&group_key, &speaker, &filtered_event.raw_text);
            if answering {
                self.group_log.mark_seen(&group_key, &ledger_key, seq);
            }
            if !unseen.is_empty() {
                let lines: Vec<String> = unseen
                    .iter()
                    .map(|(who, text)| format!("{who}: {text}"))
                    .collect();
                log_block = Some(format!(
                    "[群聊记录]\n{}\n[当前消息] {speaker}:",
                    lines.join("\n")
                ));
            }
        }
        if answering {
            match log_block {
                Some(block) => lead.push(block),
                None if shared && notice.is_none() => lead.push(format!("{speaker}:")),
                None => {}
            }
        }
        // A text-only event is rendered from `raw_text` only when it has no segments; once leading
        // text becomes a segment, the message text must be one too or it would vanish.
        if !lead.is_empty()
            && filtered_event.segments.is_empty()
            && !filtered_event.raw_text.trim().is_empty()
        {
            let text = filtered_event.raw_text.clone();
            filtered_event.segments.push(text_reply(text));
        }
        for (index, text) in lead.into_iter().enumerate() {
            filtered_event.segments.insert(index, text_reply(text));
        }
        let mut user_message = match build_user_message(
            &filtered_event,
            &capabilities,
            &context_policy,
        ) {
            Ok(message) => message,
            Err(error) => {
                tracing::error!(event_id = %filtered_event.event_id, %error, "Invalid message payload");
                let asked = notice.is_none()
                    && (!kind.is_policy_governed()
                        || bot_mentioned(filtered_event.metadata.as_ref()));
                if answering && asked {
                    return PipelineResult::LlmFailed {
                        error: error.to_string(),
                        replies: vec![text_reply("消息格式无效，本轮无法处理。")],
                    };
                }
                return PipelineResult::Passed(filtered_event);
            }
        };
        // Only a turn that reaches a model needs its pictures; see `media` for why the node
        // downloads them instead of passing the platform's URLs on.
        if answering {
            crate::pipeline::media::inline_images(&mut user_message).await;
        }

        if (user_message
            .content
            .as_deref()
            .is_some_and(|content| !content.is_empty())
            || user_message.has_parts())
            && let Some(agent) = resolved_agent
        {
            // Remember what the model is shown, so a later recall of it can be noted.
            if notice.is_none() {
                self.recalls.record_turn(
                    &filtered_event.event_id,
                    &ledger_key,
                    &filtered_event.raw_text,
                );
            }

            // Let a built-in adapter show that an answer is coming (typing, a reaction) when the
            // reply policy asks for it. Spawned: platform I/O must never delay the model call.
            if reply_policy.acknowledge
                && let Some(adapter) = self.supervisor.adapters().get(&platform).await
            {
                let event = filtered_event.clone();
                tokio::spawn(async move {
                    if let Err(err) = adapter.acknowledge(&event).await {
                        tracing::warn!(error = %err, "Adapter failed to acknowledge an event");
                    }
                });
            }

            // Register before waiting: a console stream or background compaction can own the
            // writer, and /stop must cancel this lane rather than leave a delayed turn behind.
            let running = self
                .turns
                .begin(instance.as_ref().map(|instance| instance.id.clone()));
            let signal = running.signal();
            let sessions = agent
                .builtin()
                .and_then(|agent| agent.session_manager())
                .cloned()
                .or_else(|| {
                    self.agent_factory
                        .as_ref()
                        .map(|factory| factory.sessions().clone())
                });
            let writing = match sessions.as_ref() {
                Some(sessions) => Some(tokio::select! {
                    biased;
                    () = signal.stopped() => return PipelineResult::Passed(filtered_event),
                    writing = sessions.write(&session_id) => writing,
                }),
                None => None,
            };
            // Skip discovery for a model that cannot use tools. The agent enforces the same
            // capability for native tools and dispatch, independently of how the turn is invoked.
            let tool_hosts = if agent
                .builtin()
                .is_some_and(|agent| agent.config().tool_calling)
            {
                self.tool_hosts(&hosts, instance.as_ref(), &filtered_event.event_id)
                    .await
            } else {
                tracing::debug!(
                    session_id = %session_id,
                    "Model catalog disables tool calling; offering no plugin tools"
                );
                Vec::new()
            };

            // A shared or observed group session puts other members' words into this turn's
            // context, and they could steer an administrator's shell. The turn records that fact
            // and the Bash gate refuses it unless the instance explicitly allows shared contexts.
            let bash_caller = bash_sender.map(|id| crate::BashCaller {
                id,
                instance: instance.as_ref().map(|instance| instance.id.clone()),
                shared_context: shared || observing,
            });
            // The turn writes its messages as it goes; holding the session's lock keeps a plugin
            // from deleting the conversation or appending to it in between. Plugins only ever
            // try the lock, so a tool call of this very turn cannot deadlock on it.
            let turn = self.run_conversation_turn(
                ConversationTurn {
                    agent,
                    running: &running,
                    session_id: &session_id,
                    event: &filtered_event,
                    hosts: &hosts,
                    tool_hosts,
                    bash_caller,
                    options: kanon_llm::TurnOptions::default(),
                },
                user_message,
            );
            let result = match writing {
                Some(writing) => writing.scope(turn).await,
                None => turn.await,
            };
            match result {
                Ok(output) => {
                    // Reasoning already has its own channel and parsed tool calls were removed by
                    // the agent. Delimiters or tool markup left in answer text are literal
                    // content, so delivery must not reinterpret them.
                    let answer = visible_reply(&output.content);
                    let answer = answer.as_str();
                    let mut replies = Vec::new();
                    if !answer.is_empty() {
                        replies.push(MessageSegment {
                            segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                                content: answer.to_string(),
                            })),
                        });
                    }

                    // Rich media produced by a tool (an MCP server drawing a B50 card, a TTS voice
                    // clip, a generated PDF) travels as its own segment of the kind its MIME type
                    // names, limited to the kinds the platform's adapter declares it can send.
                    if !output.attachments.is_empty() {
                        let declared = self
                            .supervisor
                            .resolve_adapter(&filtered_event.platform)
                            .await
                            .map(|route| route.capabilities());
                        let media = crate::pipeline::attachment::attachment_segments(
                            &output.attachments,
                            declared.as_deref(),
                        );
                        replies.extend(media.segments);
                        if !media.notes.is_empty() {
                            replies.push(text_reply(media.notes.join("\n")));
                        }
                    }

                    // Tool media is a complete reply even when the model produced only reasoning.
                    if replies.is_empty() {
                        tracing::debug!(
                            "LLM produced no user-visible answer or attachment; passing downstream"
                        );
                        return PipelineResult::Passed(filtered_event);
                    }

                    // Opt-in: the reasoning goes first as its own plain-text segment, content only.
                    // It is checked after the empty-reply gate so reasoning never becomes a reply
                    // on its own.
                    if reply_policy.send_reasoning {
                        let reasoning = output
                            .reasoning
                            .as_deref()
                            .unwrap_or_default()
                            .trim()
                            .to_string();
                        if !reasoning.is_empty() {
                            replies.insert(
                                0,
                                MessageSegment {
                                    segment: Some(Segment::Text(kanon_proto::v1::TextSegment {
                                        content: reasoning,
                                    })),
                                },
                            );
                        }
                    }

                    // The bot's own words belong to the group's record too, and this session has
                    // just seen them.
                    if observing {
                        let seq = self.group_log.record(&group_key, "你", answer);
                        self.group_log.mark_seen(&group_key, &ledger_key, seq);
                    }

                    hooks::emit_event(
                        &hosts,
                        EventKind::LlmResponse,
                        Detail::LlmResponse(LlmResponseEvent {
                            context: Some(filtered_event.clone()),
                            content: answer.to_string(),
                        }),
                    );
                    // Decoration changes only what is delivered; memory keeps the model's own
                    // words, so the conversation the model sees stays exactly what it said.
                    let mut replies = hooks::decorate_reply(
                        &hosts,
                        &filtered_event,
                        ReplySource::Llm,
                        "",
                        replies,
                    )
                    .await;

                    // The platform adapter turns this into its native quote of the triggering
                    // message; a reply a decorator suppressed gets no lone quote.
                    if quote_reply && !replies.is_empty() {
                        replies.insert(
                            0,
                            MessageSegment {
                                segment: Some(Segment::Reply(kanon_proto::v1::ReplySegment {
                                    target_message_id: filtered_event.event_id.clone(),
                                    snippet: String::new(),
                                })),
                            },
                        );
                    }

                    self.observe(PipelineStage::LlmReplied {
                        event_id,
                        session_id,
                        content_length: answer.chars().count(),
                    });
                    return PipelineResult::LlmReplied {
                        content: answer.to_string(),
                        replies,
                        split_lines: reply_policy.split_lines,
                    };
                }
                Err(kanon_llm::ToolRouterError::Stopped) => {
                    tracing::info!(
                        session_id = %session_id,
                        "Turn stopped by /stop; nothing is sent for it"
                    );
                }
                Err(e) => {
                    tracing::error!(
                        session_id = %session_id,
                        error = %e,
                        "LLM reasoning and tool execution failed"
                    );
                    // Someone who asked hears that no answer is coming instead of waiting for one.
                    // A turn the bot started on its own (a sampled group message, a notice) fails
                    // quietly: while a provider is down, every such turn would fail, and a notice
                    // for each would flood the group.
                    let asked = notice.is_none()
                        && (!kind.is_policy_governed()
                            || bot_mentioned(filtered_event.metadata.as_ref()));
                    if asked {
                        return PipelineResult::LlmFailed {
                            replies: vec![text_reply(failure_notice(&e))],
                            error: e.to_string(),
                        };
                    }
                }
            }
        }

        PipelineResult::Passed(filtered_event)
    }
}
