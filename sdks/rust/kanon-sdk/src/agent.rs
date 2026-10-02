//! Running the node's agent (model plus tool loop) on behalf of a plugin
//! (`BotApiService.RunAgent`).
//!
//! ```ignore
//! // A private run with tools, answering for the chat `event` came from:
//! let reply = event.agent("Find tomorrow's forecast for Paris").use_tools().await?;
//! // A turn inside the chat's conversation, remembered like any other:
//! let reply = core.agent("Greet the new member").event(&event).in_conversation().await?;
//! Ok(reply) // text plus any images or files the tools produced
//! ```
//!
//! Unlike [`CoreHandle::request_llm`], an agent run can call tools and can run inside a chat's
//! conversation.

use std::future::{Future, IntoFuture};
use std::pin::Pin;

use kanon_proto::v1::message_segment::Segment;
use kanon_proto::v1::{
    AudioSegment, FileSegment, ImageSegment, MessageSegment, RunAgentRequest, TextSegment,
    ToolAttachment, VideoSegment, audio_segment, file_segment, image_segment, video_segment,
};

use crate::context::{AsEvent, CoreHandle, validate_image};
use crate::error::CoreError;
use crate::segment::IntoReply;

/// Anything usable as an image for the model: an [`ImageSegment`] (e.g. from
/// [`MessageEvent::images`](crate::event::MessageEvent::images)) or an image
/// [`MessageSegment`] (e.g. from [`segment::image_bytes`](crate::segment::image_bytes)).
pub trait IntoImage {
    /// The image, or why this is not one.
    fn into_image(self) -> Result<ImageSegment, CoreError>;
}

impl IntoImage for ImageSegment {
    fn into_image(self) -> Result<ImageSegment, CoreError> {
        Ok(self)
    }
}

impl IntoImage for &ImageSegment {
    fn into_image(self) -> Result<ImageSegment, CoreError> {
        Ok(self.clone())
    }
}

impl IntoImage for MessageSegment {
    fn into_image(self) -> Result<ImageSegment, CoreError> {
        match self.segment {
            Some(Segment::Image(image)) => Ok(image),
            other => Err(CoreError::invalid(format!(
                "not an image segment: {other:?}"
            ))),
        }
    }
}

/// A prepared agent run; set options with the builder methods, then `.await` it.
///
/// Mistakes in the options (an empty prompt, a conversation run without a chat, a malformed
/// model reference) are reported when awaited, as [`CoreError::InvalidArgument`], before
/// anything is sent.
#[must_use = "an agent run does nothing until awaited"]
pub struct AgentRequest {
    core: Option<CoreHandle>,
    request: RunAgentRequest,
    /// The first builder mistake, reported when the run is awaited (builders cannot fail).
    invalid: Option<CoreError>,
}

impl std::fmt::Debug for AgentRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentRequest")
            .field("request", &self.request)
            .field("invalid", &self.invalid)
            .finish_non_exhaustive()
    }
}

impl AgentRequest {
    /// A run of `prompt` through `core` (`None` in standalone mode, which fails when awaited).
    pub(crate) fn new(core: Option<CoreHandle>, prompt: String) -> Self {
        Self {
            core,
            request: RunAgentRequest {
                prompt,
                ..Default::default()
            },
            invalid: None,
        }
    }

    /// The chat the run serves: it picks the bot instance (model, tools, policies) and is passed
    /// to tools as their context.
    pub fn event(mut self, event: &impl AsEvent) -> Self {
        self.request.context = Some(event.as_event().clone());
        self
    }

    /// Runs inside the chat's current conversation, with its history and persona, and appends
    /// the turn to it — exactly as if the model had answered a message. Needs
    /// [`event`](Self::event); without this the run uses a private session discarded afterwards.
    pub fn in_conversation(mut self) -> Self {
        self.request.in_conversation = true;
        self
    }

    /// Lets the agent call tools (those the instance allows: plugins, MCP, built-in).
    pub fn use_tools(mut self) -> Self {
        self.request.use_tools = true;
        self
    }

    /// Instructions for a private run. A conversation's system prompt is fixed, so combining
    /// this with [`in_conversation`](Self::in_conversation) is rejected.
    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.request.system_prompt = system_prompt.into();
        self
    }

    /// The model as `<provider>/<model-id>`; without it the instance's model, else the node's
    /// default model, answers.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.request.model = model.into();
        self
    }

    /// Tool rounds allowed; without it the agent's default applies.
    pub fn max_steps(mut self, max_steps: u32) -> Self {
        self.request.max_steps = max_steps;
        self
    }

    /// Adds an image for the agent to look at; call repeatedly for several.
    pub fn image(mut self, image: impl IntoImage) -> Self {
        match image.into_image() {
            Ok(image) => self.request.images.push(image),
            Err(err) => {
                self.invalid.get_or_insert(err);
            }
        }
        self
    }

    /// Adds several images, e.g. `event.images()`.
    pub fn images<I: IntoImage>(self, images: impl IntoIterator<Item = I>) -> Self {
        images.into_iter().fold(self, Self::image)
    }

    /// Validates the options and runs the agent.
    async fn run(self) -> Result<AgentReply, CoreError> {
        if let Some(err) = self.invalid {
            return Err(err);
        }
        let core = self.core.ok_or(CoreError::Standalone)?;
        let mut request = self.request;
        request.plugin_id = core.require_plugin_id("agent")?;
        if request.prompt.trim().is_empty() && request.images.is_empty() {
            return Err(CoreError::invalid("an agent run needs a prompt or images"));
        }
        if request.in_conversation && request.context.is_none() {
            return Err(CoreError::invalid(
                "in_conversation needs the chat: set .event(..)",
            ));
        }
        if request.in_conversation && !request.system_prompt.is_empty() {
            return Err(CoreError::invalid(
                "a conversation keeps its own system prompt; system_prompt is for private runs",
            ));
        }
        if !request.model.is_empty() {
            match request.model.split_once('/') {
                Some((provider, id)) if !provider.is_empty() && !id.is_empty() => {}
                _ => {
                    return Err(CoreError::invalid(format!(
                        "model '{}' must be written as <provider>/<model-id>",
                        request.model
                    )));
                }
            }
        }
        request.images.iter().try_for_each(validate_image)?;
        let response = core.run_agent(request).await?;
        Ok(AgentReply {
            text: response.content,
            attachments: response.attachments,
            tools: response.tools,
            session_id: response.session_id,
        })
    }
}

impl IntoFuture for AgentRequest {
    type Output = Result<AgentReply, CoreError>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

/// What an agent run produced.
///
/// Return it from a command handler to send the answer: it converts into the text followed by
/// the attachments, as images, voice, video or files by their MIME type.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentReply {
    /// The final answer, with the model's reasoning removed.
    pub text: String,
    /// Images and files the tools produced.
    pub attachments: Vec<ToolAttachment>,
    /// Tools called, in order.
    pub tools: Vec<String>,
    /// The session the run used: the conversation's, or the discarded private one.
    pub session_id: String,
}

impl IntoReply for AgentReply {
    fn into_segments(self) -> Vec<MessageSegment> {
        let mut segments = Vec::new();
        if !self.text.is_empty() {
            segments.push(wrap(Segment::Text(TextSegment { content: self.text })));
        }
        segments.extend(self.attachments.into_iter().filter_map(attachment_segment));
        segments
    }
}

fn wrap(segment: Segment) -> MessageSegment {
    MessageSegment {
        segment: Some(segment),
    }
}

/// The segment for one tool attachment, chosen by MIME type the way the core sends a turn's
/// attachments; `None` (logged) for an attachment with neither a file nor a URL.
fn attachment_segment(attachment: ToolAttachment) -> Option<MessageSegment> {
    enum Location {
        Path(String),
        Url(String),
    }
    let location = match (attachment.file_path, attachment.url) {
        (Some(path), _) if !path.is_empty() => Location::Path(path),
        (_, Some(url)) if !url.is_empty() => Location::Url(url),
        _ => {
            tracing::warn!(mime = %attachment.mime_type, "Agent attachment has no file or URL; skipped");
            return None;
        }
    };
    let mime = attachment.mime_type;
    let segment = if mime.starts_with("image/") {
        Segment::Image(ImageSegment {
            source: Some(match location {
                Location::Path(path) => image_segment::Source::FilePath(path),
                Location::Url(url) => image_segment::Source::Url(url),
            }),
            mime_type: Some(mime),
            filename: None,
        })
    } else if mime.starts_with("audio/") {
        Segment::Audio(AudioSegment {
            source: Some(match location {
                Location::Path(path) => audio_segment::Source::FilePath(path),
                Location::Url(url) => audio_segment::Source::Url(url),
            }),
            duration_seconds: None,
        })
    } else if mime.starts_with("video/") {
        Segment::Video(VideoSegment {
            source: Some(match location {
                Location::Path(path) => video_segment::Source::FilePath(path),
                Location::Url(url) => video_segment::Source::Url(url),
            }),
            mime_type: Some(mime),
            filename: None,
        })
    } else {
        let (name, source) = match location {
            Location::Path(path) => (file_name(&path), file_segment::Source::FilePath(path)),
            Location::Url(url) => (file_name(&url), file_segment::Source::Url(url)),
        };
        Segment::File(FileSegment {
            source: Some(source),
            name,
        })
    };
    Some(wrap(segment))
}

/// The last path component of a file path or URL, which recipients see as the file name.
fn file_name(location: &str) -> String {
    let path = location.split(['?', '#']).next().unwrap_or(location);
    match path.rsplit(['/', '\\']).next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "attachment".to_string(),
    }
}
