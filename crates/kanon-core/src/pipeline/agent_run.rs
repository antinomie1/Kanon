//! The node's agent, run on a plugin's behalf (`RunAgent`).
//!
//! A plugin can have the agent answer a prompt in one of two places:
//!
//! - **inside a chat's current conversation** (`in_conversation`): the turn is answered with the
//!   conversation's history, persona and plugins, and appended to it, exactly like a turn the
//!   pipeline runs for an inbound message. It goes through
//!   [`PipelineEngine::run_conversation_turn`], so subscribers hear `AGENT_BEGIN`/`AGENT_DONE`,
//!   prompt rewriters take part, and `/stop` reaches it. Like every external writer it only
//!   proceeds when it can take the session's lock at once (see [`kanon_llm::SessionManager`]).
//! - **in a private session** (the default): a one-off agent with its own empty memory (see
//!   [`kanon_llm::AgentFactory::private_agent`]), whose session vanishes with it. No plugin hook
//!   runs, so a plugin calling `RunAgent` from its own hook cannot recurse into itself.
//!
//! Either way the run never executes Bash: a plugin names the chat by a message it holds, and a
//! sender identity it could have written itself must not unlock a shell.

use std::sync::atomic::{AtomicU64, Ordering};

use kanon_llm::{
    ChatMessage, ContentPart, ModelRef, ToolAttachment, ToolRouter, ToolRouterError, TurnOptions,
    visible_reply,
};
use kanon_proto::v1::PipelineEventRequest;

use super::conversations::ConversationError;
use super::engine::{ConversationTurn, PipelineEngine};

/// Private sessions are named `plugin:<plugin_id>:<n>`, numbered from this counter.
static PRIVATE_RUNS: AtomicU64 = AtomicU64::new(1);

/// One agent run a plugin asked for.
#[derive(Debug, Clone, Default)]
pub struct AgentRun {
    /// The plugin asking; names the private session.
    pub plugin_id: String,
    /// The user message for the agent.
    pub prompt: String,
    /// Images for the model, sent with this turn only.
    pub images: Vec<ContentPart>,
    /// The chat the run serves: its instance decides the model, plugins and tools, and tools
    /// receive it as their context. Required when `in_conversation`.
    pub context: Option<PipelineEventRequest>,
    /// Answer inside the chat's current conversation instead of a private session.
    pub in_conversation: bool,
    /// Instructions replacing the persona of a private run; ignored in a conversation.
    pub instructions: Option<String>,
    /// `<provider>/<model-id>` to answer with; `None` uses the instance's model, else the node's.
    pub model: Option<String>,
    /// Whether the model is offered tools at all.
    pub use_tools: bool,
    /// Tool rounds allowed; `None` uses the agent's default.
    pub max_steps: Option<usize>,
}

/// What a finished run produced.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRunOutput {
    /// The final answer, reasoning removed.
    pub content: String,
    /// Media the run's tools produced, for the plugin to send.
    pub attachments: Vec<ToolAttachment>,
    /// Tools called, in order.
    pub tools: Vec<String>,
    /// The session the run used: the conversation's, or the discarded private one.
    pub session_id: String,
}

/// Why a run did not produce an answer.
#[derive(Debug, thiserror::Error)]
pub enum AgentRunError {
    /// The chat or its conversation could not be used (no instance claims it, it is busy, no
    /// model is configured, ...).
    #[error(transparent)]
    Conversation(#[from] ConversationError),
    /// The requested model does not resolve to a configured provider.
    #[error("model '{0}' does not resolve to a configured provider")]
    UnknownModel(String),
    /// The request itself is malformed.
    #[error("{0}")]
    Invalid(String),
    /// The run was stopped (`/stop` in the chat it serves) before it finished.
    #[error("the run was stopped before it finished")]
    Stopped,
    /// The model or a tool failed.
    #[error("the agent failed: {0}")]
    Failed(ToolRouterError),
}

impl PipelineEngine {
    /// Runs the node's agent for a plugin; see the module documentation.
    pub async fn run_agent(&self, run: AgentRun) -> Result<AgentRunOutput, AgentRunError> {
        if run.prompt.trim().is_empty() && run.images.is_empty() {
            return Err(AgentRunError::Invalid(
                "the prompt is empty and no image was given".to_string(),
            ));
        }
        if run.in_conversation && run.context.is_none() {
            return Err(AgentRunError::Invalid(
                "a run in a conversation must name its chat by an inbound message as `context`"
                    .to_string(),
            ));
        }
        let factory = self
            .agent_factory()
            .filter(|factory| factory.node_agent().is_some())
            .ok_or(ConversationError::NoModel)?
            .clone();

        // The chat decides the instance, and with it the default model and the plugins.
        let chat = match &run.context {
            Some(event) => Some(self.resolve_chat(event).await?),
            None => None,
        };
        let instance = chat.as_ref().map(|chat| &chat.instance);
        let model = run
            .model
            .clone()
            .filter(|model| !model.trim().is_empty())
            .or_else(|| instance.and_then(|instance| instance.model.clone()));
        let unknown_model = || AgentRunError::UnknownModel(model.clone().unwrap_or_default());

        let (agent, session_id) = match (&chat, run.in_conversation) {
            (Some(chat), true) => {
                let agent = factory
                    .agent_for_model(model.as_deref())
                    .ok_or_else(unknown_model)?;
                let session_id = chat.instance.conversation_session_id(&chat.conversation);
                (agent, session_id)
            }
            _ => {
                let agent = factory
                    .private_agent(model.as_deref(), run.instructions.clone())
                    .ok_or_else(unknown_model)?;
                let n = PRIVATE_RUNS.fetch_add(1, Ordering::Relaxed);
                (agent, format!("plugin:{}:{n}", run.plugin_id))
            }
        };

        let capabilities = factory
            .models()
            .settings_for(&ModelRef::parse(&agent.config().model_ref()))
            .capabilities;
        if !run.images.is_empty() && !capabilities.vision {
            return Err(AgentRunError::Invalid(format!(
                "model '{}' is not set up to accept images",
                agent.config().model_ref()
            )));
        }
        let mut message = if run.images.is_empty() {
            ChatMessage::user(run.prompt)
        } else {
            ChatMessage::user_multimodal(run.prompt, run.images)
        };
        // Remote images are fetched by the node, as for inbound messages (see `media`).
        super::media::inline_images(&mut message).await;

        let hosts = match instance {
            Some(instance) => self.hosts_of(instance).await,
            None => self.enabled_hosts().await,
        };
        let event_id = run
            .context
            .as_ref()
            .map(|event| event.event_id.clone())
            .unwrap_or_default();
        let tool_hosts = if run.use_tools && capabilities.tool_calling {
            self.tool_hosts(&hosts, instance, &event_id).await
        } else {
            Vec::new()
        };
        let options = TurnOptions {
            max_iterations: run.max_steps,
            without_tools: !run.use_tools,
        };

        let result = match (&chat, run.in_conversation, &run.context) {
            (Some(chat), true, Some(event)) => {
                let writing = factory
                    .sessions()
                    .try_write(&session_id)
                    .map_err(|_| ConversationError::Busy(session_id.clone()))?;
                // The instance decides the persona, as for the pipeline's own turns.
                if let Some(persona_id) = chat.instance.effective_persona_id()
                    && let Some(sessions) = agent.session_manager()
                {
                    factory
                        .personas()
                        .with_persona(&persona_id, |_| {
                            sessions.set_persona(&session_id, &persona_id)
                        })
                        .ok_or_else(|| {
                            ConversationError::Invalid(format!(
                                "persona '{persona_id}' no longer exists"
                            ))
                        })?
                        .map_err(|error| ConversationError::Storage(error.to_string()))?;
                }
                writing
                    .scope(self.run_conversation_turn(
                        ConversationTurn {
                            agent,
                            running: self.running_turns().begin(Some(chat.instance.id.clone())),
                            session_id: &session_id,
                            event,
                            hosts: &hosts,
                            tool_hosts,
                            bash_caller: None,
                            options,
                        },
                        message,
                    ))
                    .await
            }
            _ => {
                // Registered under the chat's instance, so `/stop` there ends it too.
                let running = self
                    .running_turns()
                    .begin(instance.map(|instance| instance.id.clone()));
                let router = ToolRouter::from_arc(agent);
                let turn = kanon_llm::with_stop_signal(
                    running.signal(),
                    router.execute_message_with(&session_id, message, &tool_hosts, options),
                );
                match run.context.clone() {
                    // Tools still learn which chat they serve; no plugin hook runs.
                    Some(event) => crate::supervisor::with_tool_event(event, turn).await,
                    None => turn.await,
                }
            }
        };

        match result {
            Ok(output) => Ok(AgentRunOutput {
                content: visible_reply(&output.content),
                attachments: output.attachments,
                tools: output
                    .executed_tools
                    .iter()
                    .map(|call| call.tool_name.clone())
                    .collect(),
                session_id,
            }),
            Err(ToolRouterError::Stopped) => Err(AgentRunError::Stopped),
            Err(err) => Err(AgentRunError::Failed(err)),
        }
    }
}
