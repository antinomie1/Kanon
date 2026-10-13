//! Built-in conversation and operator commands.

use super::*;

impl PipelineEngine {
    /// Reads the model conversation that answering `event` would continue.
    ///
    /// The session is derived exactly as the model phase derives it — owning instance, group
    /// session scope, `/new` generation — so a plugin reads the same history the model would see.
    /// Read-only by construction: history is append-only and only the pipeline writes it.
    pub async fn conversation_history(
        &self,
        event: &PipelineEventRequest,
    ) -> Result<ConversationHistory, crate::pipeline::conversations::ConversationError> {
        use crate::pipeline::conversations::ConversationError;
        let instance = match &self.instances {
            Some(registry) => match registry.resolve_by_platform(&event.platform).await {
                Ok(Some(instance)) => Some(instance),
                Ok(None) => return Err(ConversationError::NoInstance(event.platform.clone())),
                Err(err) => return Err(ConversationError::Ambiguous(err.to_string())),
            },
            None => None,
        };
        let agent = match &self.agent_factory {
            Some(factory) => {
                factory.agent_for_model(instance.as_ref().and_then(|i| i.model.as_deref()))
            }
            None => self.agent.current(),
        }
        .ok_or(ConversationError::NoModel)?;

        let conversation = instance_conversation_key(event, instance.as_ref());
        let session_id = match instance.as_ref() {
            Some(instance) => instance.conversation_session_id(&conversation),
            None => conversation,
        };
        let snapshot = agent
            .memory()
            .snapshot(&session_id)
            .await
            .map_err(|err| ConversationError::Storage(err.to_string()))?;
        Ok(ConversationHistory {
            session_id,
            summary: snapshot.summary,
            messages: snapshot.messages,
        })
    }

    /// Turns a plugin's command (or trigger) response into the pipeline result, recording the
    /// conversation capture the plugin asked for, or hands the message on to the model.
    ///
    /// A transport failure is logged and reported as an unsuccessful execution without replies:
    /// the plugin never answered, so there is nothing truthful to send on its behalf.
    pub(super) async fn command_result(
        &self,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
        command: String,
        plugin_id: &str,
        host: &Arc<crate::supervisor::ManagedHost>,
        event: &PipelineEventRequest,
        outcome: Result<kanon_proto::v1::CommandExecuteResponse, tonic::Status>,
    ) -> CommandFlow {
        match outcome {
            Ok(response) => {
                let pass_to_model = response.pass_to_model && response.capture_seconds == 0;
                if response.pass_to_model && !pass_to_model {
                    tracing::warn!(
                        command = %command,
                        plugin_id = %plugin_id,
                        "Plugin both captured the conversation and passed the message to the model; keeping the capture"
                    );
                }
                if response.capture_seconds > 0
                    && !self.captures.capture(
                        event,
                        &host.host_id,
                        plugin_id,
                        &command,
                        response.capture_seconds,
                    )
                {
                    tracing::warn!(
                        command = %command,
                        plugin_id = %plugin_id,
                        "Plugin asked to capture a conversation whose event names no sender; ignored"
                    );
                }
                let replies = hooks::decorate_reply(
                    hosts,
                    event,
                    ReplySource::Command,
                    &command,
                    response.replies,
                )
                .await;
                if pass_to_model {
                    tracing::debug!(
                        command = %command,
                        plugin_id = %plugin_id,
                        rewritten = response.model_text.is_some(),
                        "Plugin handed the message on to the model"
                    );
                    if !response.success {
                        tracing::warn!(
                            command = %command,
                            plugin_id = %plugin_id,
                            error = %response.error_message,
                            "Plugin reported a failure while handing the message on"
                        );
                    }
                    // The handler's own replies go out first, through the same FIFO the model's
                    // answer will take, so they arrive in the order they were produced.
                    if !replies.is_empty() {
                        self.enqueue_reply(
                            DeliverMessageRequest {
                                platform: event.platform.clone(),
                                channel_id: event.channel_id.clone(),
                                recipient_id: event.sender_id.clone(),
                                segments: replies,
                                event_id: event.event_id.clone(),
                            },
                            false,
                        );
                    }
                    let mut event = event.clone();
                    if let Some(text) = response.model_text {
                        replace_message_text(&mut event, text);
                    }
                    return CommandFlow::PassToModel(event);
                }
                CommandFlow::Finished(PipelineResult::CommandExecuted {
                    command,
                    plugin_id: plugin_id.to_string(),
                    host_id: host.host_id.clone(),
                    success: response.success,
                    replies,
                })
            }
            Err(status) => {
                tracing::error!(
                    command = %command,
                    host_id = %host.host_id,
                    error = %status,
                    "Command execution failed with gRPC status"
                );
                CommandFlow::Finished(PipelineResult::CommandExecuted {
                    command,
                    plugin_id: plugin_id.to_string(),
                    host_id: host.host_id.clone(),
                    success: false,
                    replies: vec![],
                })
            }
        }
    }

    /// Handles the built-in `/stop` command: stops every running model turn of `instance`.
    ///
    /// A stopped turn ends at its next wait on the model or a tool and sends no reply; what it
    /// already did (commands run, files written) stays done.
    pub(super) fn handle_stop_command(
        &self,
        instance: &crate::instance::BotInstance,
    ) -> PipelineResult {
        let stopped = self.turns.stop_instance(&instance.id);
        tracing::info!(
            instance_id = %instance.id,
            stopped_turns = stopped,
            "Built-in /stop stopped the instance's running turns"
        );
        let reply = if stopped == 0 {
            "当前没有正在运行的任务。".to_string()
        } else {
            format!("已停止 {stopped} 个正在运行的任务。")
        };
        PipelineResult::BuiltinReplied {
            command: STOP_COMMAND.to_string(),
            replies: vec![text_reply(reply)],
        }
    }

    /// Handles the built-in `/new` command: starts a new, empty conversation for the chat.
    ///
    /// The previous conversation is kept: `/ls` lists it and `/switch` returns to it.
    pub(super) async fn handle_new_session(
        &self,
        event: &PipelineEventRequest,
        instance: &crate::instance::BotInstance,
    ) -> PipelineResult {
        let chat = crate::pipeline::conversations::Chat {
            instance: instance.clone(),
            conversation: instance_conversation_key(event, Some(instance)),
        };
        match self.start_conversation(&chat).await {
            Ok(session_id) => {
                tracing::info!(
                    instance_id = %instance.id,
                    session_id = %session_id,
                    conversation = %chat.conversation,
                    "Built-in /new started a new conversation for this chat"
                );
                PipelineResult::SessionRotated {
                    instance_id: instance.id.clone(),
                    session_id,
                    replies: vec![text_reply("已开启新会话。")],
                }
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %instance.id,
                    error = %err,
                    "Built-in /new could not start a conversation; the chat is unchanged"
                );
                PipelineResult::BuiltinReplied {
                    command: NEW_SESSION_COMMAND.to_string(),
                    replies: vec![text_reply(format!("开启新会话失败：{err}"))],
                }
            }
        }
    }

    /// Handles `/ls`, `/switch <n>` and `/del [n]` for the chat that sent `event`.
    ///
    /// Conversations are numbered from 1 in `/ls` order (oldest first), and `/switch` and `/del`
    /// take those numbers. Every outcome, failures included, is answered in the chat.
    pub(super) async fn handle_conversation_command(
        &self,
        command: &str,
        event: &PipelineEventRequest,
        instance: &crate::instance::BotInstance,
        args: &[String],
    ) -> PipelineResult {
        let chat = crate::pipeline::conversations::Chat {
            instance: instance.clone(),
            conversation: instance_conversation_key(event, Some(instance)),
        };
        let reply = match self.conversation_command_reply(command, &chat, args).await {
            Ok(reply) => reply,
            Err(err) => {
                tracing::warn!(
                    instance_id = %instance.id,
                    command = %command,
                    error = %err,
                    "Built-in conversation command failed"
                );
                match err {
                    crate::pipeline::conversations::ConversationError::Busy(_) => {
                        "该会话正在运行任务，请稍后再试或先发送 /stop。".to_string()
                    }
                    other => format!("操作失败：{other}"),
                }
            }
        };
        PipelineResult::BuiltinReplied {
            command: command.to_string(),
            replies: vec![text_reply(reply)],
        }
    }

    /// The chat reply of one conversation command.
    pub(super) async fn conversation_command_reply(
        &self,
        command: &str,
        chat: &crate::pipeline::conversations::Chat,
        args: &[String],
    ) -> Result<String, crate::pipeline::conversations::ConversationError> {
        let conversations = self.chat_conversations(chat).await?;
        if command == LIST_SESSIONS_COMMAND {
            return Ok(render_conversation_list(&conversations));
        }
        // `/del` without a number means the current conversation; `/switch` needs one.
        let index = match args.first() {
            Some(arg) => match arg.trim().parse::<usize>() {
                Ok(index) if (1..=conversations.len()).contains(&index) => Some(index),
                _ => {
                    return Ok(format!(
                        "没有第 {} 个会话（共 {} 个），发送 /ls 查看序号。",
                        arg.trim(),
                        conversations.len()
                    ));
                }
            },
            None => None,
        };
        if command == SWITCH_SESSION_COMMAND {
            let Some(index) = index else {
                return Ok("用法：/switch <序号>，序号见 /ls。".to_string());
            };
            let target = &conversations[index - 1];
            if target.current {
                return Ok(format!("会话 {index} 已是当前会话。"));
            }
            self.switch_conversation(chat, &target.session_id).await?;
            return Ok(format!(
                "已切换到会话 {index}：{}",
                display_title(&target.title)
            ));
        }
        let index = index.unwrap_or_else(|| {
            conversations
                .iter()
                .position(|conversation| conversation.current)
                .map_or(1, |position| position + 1)
        });
        let target = self
            .delete_conversation(chat, &conversations[index - 1].session_id)
            .await?;
        let mut reply = format!("已删除会话 {index}：{}", display_title(&target.title));
        if target.current {
            reply.push_str("\n已开启新会话。");
        }
        Ok(reply)
    }

    /// Handles the built-in `/model` command for one instance.
    ///
    /// With no argument — or one that is not a valid index — it lists the models an operator may
    /// switch to. With a valid 1-based index it persists the choice on the instance, so a restart
    /// does not silently move the conversation back to the previous model.
    pub(super) async fn handle_model_command(
        &self,
        instance: &crate::instance::BotInstance,
        args: &[String],
    ) -> PipelineResult {
        let options = self.model_options(instance);
        let current = instance
            .model
            .clone()
            .or_else(|| self.node_model_reference());

        let selected = args
            .first()
            .and_then(|arg| arg.trim().parse::<usize>().ok())
            .filter(|index| *index >= 1 && *index <= options.len());

        let Some(index) = selected else {
            return PipelineResult::ModelListed {
                instance_id: instance.id.clone(),
                count: options.len(),
                replies: vec![text_reply(render_model_list(&options, current.as_deref()))],
            };
        };

        let target = options[index - 1].0.clone();
        let Some(registry) = self.instances.as_ref() else {
            // Unreachable in the node (the instance gate implies a registry), but a missing
            // registry must not silently pretend the switch succeeded.
            tracing::error!(
                instance_id = %instance.id,
                "Built-in /model received without an instance catalog; ignoring command"
            );
            return PipelineResult::ModelListed {
                instance_id: instance.id.clone(),
                count: options.len(),
                replies: vec![text_reply(render_model_list(&options, current.as_deref()))],
            };
        };

        let providers = self
            .agent_factory
            .as_ref()
            .map(|factory| factory.providers().as_ref());
        match registry
            .set_model(&instance.id, Some(target.clone()), providers)
            .await
        {
            Ok(_) => {
                tracing::info!(
                    instance_id = %instance.id,
                    model = %target,
                    "Built-in /model switched the instance model"
                );
                PipelineResult::ModelSelected {
                    instance_id: instance.id.clone(),
                    model: target.clone(),
                    replies: vec![text_reply(format!("已切换当前实例模型为 {target}。"))],
                }
            }
            Err(err) => {
                tracing::error!(
                    instance_id = %instance.id,
                    model = %target,
                    error = %err,
                    "Failed to persist the model selected by built-in /model"
                );
                PipelineResult::ModelListed {
                    instance_id: instance.id.clone(),
                    count: options.len(),
                    replies: vec![text_reply(format!("切换模型失败：{err}"))],
                }
            }
        }
    }

    /// Selectable models, in listing order, with their catalog settings when known.
    ///
    /// The node's default model and the instance's current model are always present even when the
    /// catalog is empty, so `/model` stays useful on a freshly configured node.
    pub(super) fn model_options(
        &self,
        instance: &crate::instance::BotInstance,
    ) -> Vec<(String, Option<ModelSpec>)> {
        let mut options: Vec<(String, Option<ModelSpec>)> = self
            .agent_factory
            .as_ref()
            .map(|factory| {
                factory
                    .models()
                    .list()
                    .into_iter()
                    .map(|spec| (spec.full_name(), Some(spec)))
                    .collect()
            })
            .unwrap_or_default();

        for extra in [self.node_model_reference(), instance.model.clone()]
            .into_iter()
            .flatten()
        {
            if !options.iter().any(|(reference, _)| reference == &extra) {
                options.push((extra, None));
            }
        }

        options
    }

    /// Canonical model reference of the node's default agent, when configured.
    pub(super) fn node_model_reference(&self) -> Option<String> {
        self.agent_factory
            .as_ref()
            .and_then(|factory| factory.default_model())
            .or_else(|| self.agent.current().map(|agent| agent.config().model_ref()))
    }

    /// Answers the built-in `/help` command.
    ///
    /// Lists the core's own commands first and then every command the active plugin hosts declare,
    /// so one message tells a user everything they can type without opening the console.
    pub(super) fn handle_help_command(
        &self,
        hosts: &[Arc<crate::supervisor::ManagedHost>],
    ) -> PipelineResult {
        let mut rendered = String::from("内置指令：\n");
        rendered.push_str("/new — 开始新会话\n");
        rendered.push_str("/ls — 列出本聊天的会话\n");
        rendered.push_str("/switch <序号> — 切换到另一个会话\n");
        rendered.push_str("/del [序号] — 删除会话（默认当前会话）\n");
        rendered.push_str("/model — 列出可用模型；/model <序号> 切换当前实例模型\n");
        rendered.push_str("/stop — 停止当前实例正在运行的任务\n");
        rendered.push_str("/help — 显示本帮助\n");
        rendered.push_str("/info — 显示系统与运行信息\n");

        // Command names are deduplicated: two plugins claiming the same name would otherwise show
        // up twice, while routing already resolves that collision deterministically.
        let mut plugin_commands: Vec<(String, String, String, Vec<String>, Vec<CommandMeta>)> =
            Vec::new();
        let mut triggers: Vec<(String, String)> = Vec::new();
        for host in hosts {
            for plugin in host.metas() {
                for command in &plugin.commands {
                    let name = command.name.trim().trim_start_matches('/').to_string();
                    if name.is_empty() || plugin_commands.iter().any(|(seen, ..)| *seen == name) {
                        continue;
                    }
                    let aliases = command
                        .aliases
                        .iter()
                        .map(|alias| alias.trim().trim_start_matches('/').to_string())
                        .filter(|alias| !alias.is_empty())
                        .collect();
                    plugin_commands.push((
                        name,
                        command.description.trim().to_string(),
                        command.usage.trim().to_string(),
                        aliases,
                        command.subcommands.clone(),
                    ));
                }
                // A trigger without a description is an implementation detail of its plugin, not
                // something a user can usefully be told about.
                for trigger in &plugin.triggers {
                    let description = trigger.description.trim();
                    if !description.is_empty()
                        && !triggers.iter().any(|(name, _)| *name == trigger.name)
                    {
                        triggers.push((trigger.name.clone(), description.to_string()));
                    }
                }
            }
        }

        if !plugin_commands.is_empty() {
            plugin_commands.sort_by(|left, right| left.0.cmp(&right.0));
            rendered.push_str("\n插件指令：\n");
            for (name, description, usage, aliases, subcommands) in plugin_commands {
                rendered.push('/');
                rendered.push_str(&name);
                if !aliases.is_empty() {
                    rendered.push_str("（别名: /");
                    rendered.push_str(&aliases.join(", /"));
                    rendered.push('）');
                }
                if !description.is_empty() {
                    rendered.push_str(" — ");
                    rendered.push_str(&description);
                }
                if !usage.is_empty() {
                    rendered.push_str("（用法: ");
                    rendered.push_str(&usage);
                    rendered.push('）');
                }
                rendered.push('\n');
                // A command group lists its subcommands under it, indented, in declaration order:
                // routing still goes to the group (`/name sub args`) and the plugin dispatches.
                // Like a command's, a subcommand's usage is the full line (`/todo add <text>`).
                for sub in &subcommands {
                    let sub_name = sub.name.trim();
                    if sub_name.is_empty() {
                        continue;
                    }
                    rendered.push_str("  ");
                    let sub_usage = sub.usage.trim();
                    if sub_usage.is_empty() {
                        rendered.push('/');
                        rendered.push_str(&name);
                        rendered.push(' ');
                        rendered.push_str(sub_name);
                    } else {
                        rendered.push_str(sub_usage);
                    }
                    let sub_description = sub.description.trim();
                    if !sub_description.is_empty() {
                        rendered.push_str(" — ");
                        rendered.push_str(sub_description);
                    }
                    rendered.push('\n');
                }
            }
        }

        if !triggers.is_empty() {
            triggers.sort();
            rendered.push_str("\n消息触发：\n");
            for (_, description) in triggers {
                rendered.push_str(&description);
                rendered.push('\n');
            }
        }

        PipelineResult::BuiltinReplied {
            command: HELP_COMMAND.to_string(),
            replies: vec![text_reply(rendered)],
        }
    }

    /// Answers the built-in `/info` command.
    ///
    /// Deliberately short: platform, one line of host facts, local time and the model this
    /// conversation is served by. Anything more belongs in the console, not in a chat message.
    pub(super) fn handle_info_command(
        &self,
        instance: Option<&crate::instance::BotInstance>,
        event: &PipelineEventRequest,
    ) -> PipelineResult {
        let now = crate::time::now_unix();
        let timezone = match crate::time::local_offset_seconds(now) {
            Some(offset) => format!(" {}", crate::time::format_offset(offset)),
            None => String::new(),
        };

        let mut rendered = String::new();
        #[cfg(target_os = "macos")]
        {
            // Separate the product version from the Darwin kernel release on the same line.
            let architecture = match std::env::consts::ARCH {
                "aarch64" => "ARM64 (aarch64)",
                "x86_64" => "x86-64 (x86_64)",
                architecture => architecture,
            };
            rendered.push_str(&format!(
                "系统: {} | Kernel: Darwin {} | Arch: {architecture}\n",
                distribution_name(),
                kernel_release().unwrap_or_else(|| "unknown".to_string())
            ));
        }
        #[cfg(not(target_os = "macos"))]
        rendered.push_str(&format!(
            "系统: {} {} ({})\n",
            distribution_name(),
            kernel_release().unwrap_or_default(),
            std::env::consts::ARCH
        ));
        rendered.push_str(&format!(
            "时间: {}{timezone}\n",
            crate::time::format_local(now)
        ));
        if let Some(instance) = instance {
            rendered.push_str(&format!("实例: {} ({})\n", instance.name, instance.id));
        }
        let model = instance
            .and_then(|instance| instance.model.clone())
            .or_else(|| self.node_model_reference())
            .unwrap_or_else(|| "未配置".to_string());
        rendered.push_str(&format!("模型: {model}\n"));
        rendered.push_str(&format!("适配器: {}", event.platform));

        PipelineResult::BuiltinReplied {
            command: INFO_COMMAND.to_string(),
            replies: vec![text_reply(rendered)],
        }
    }
}
