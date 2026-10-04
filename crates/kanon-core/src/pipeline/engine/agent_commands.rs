//! Commands whose state belongs to an optional external agent.

use super::*;
use crate::instance::BotInstance;
use kanon_llm::dsh::DshClient;
use serde_json::Value;

impl PipelineEngine {
    pub(super) async fn dsh_model_command(
        &self,
        client: &DshClient,
        instance: &BotInstance,
        event: &PipelineEventRequest,
        args: &[String],
    ) -> Result<PipelineResult, crate::pipeline::conversations::ConversationError> {
        use crate::pipeline::conversations::ConversationError;
        let catalog = client.models().await?;
        let groups = catalog["groups"]
            .as_array()
            .ok_or_else(|| ConversationError::Invalid("DSH model catalog has no groups".into()))?;
        let mut options = Vec::new();
        for group in groups {
            let provider = group["id"]
                .as_str()
                .ok_or_else(|| ConversationError::Invalid("DSH provider has no id".into()))?;
            let models = group["models"].as_array().ok_or_else(|| {
                ConversationError::Invalid("DSH provider has no model list".into())
            })?;
            for model in models {
                let id = model["id"]
                    .as_str()
                    .ok_or_else(|| ConversationError::Invalid("DSH model has no id".into()))?;
                options.push((provider.to_string(), id.to_string()));
            }
        }
        let conversation = instance_conversation_key(event, Some(instance));
        let session_id = instance.dsh_session_id_at(
            &conversation,
            instance.dsh_session_generation(&conversation),
        );
        let selected = args
            .first()
            .and_then(|arg| arg.parse::<usize>().ok())
            .filter(|index| (1..=options.len()).contains(index));
        if let Some(index) = selected {
            let sessions = self.session_manager().ok_or(ConversationError::NoModel)?;
            let _writing = sessions
                .try_write(&session_id)
                .map_err(|_| ConversationError::Busy(session_id.clone()))?;
            let (provider, model) = &options[index - 1];
            client.create_session(&session_id, None).await?;
            client
                .select_model(&session_id, provider, model, None)
                .await?;
            let model = format!("{provider}/{model}");
            return Ok(PipelineResult::ModelSelected {
                instance_id: instance.id.clone(),
                model: model.clone(),
                replies: vec![text_reply(format!("已切换当前 DSH 会话模型为 {model}。"))],
            });
        }
        let projections: Value = client
            .call(
                "session/projections",
                serde_json::json!({"request": {"sessionId": session_id}}),
            )
            .await?;
        let next = &projections["values"]["modelSelection"]["next"];
        let current = if next.is_null() {
            &catalog["default"]
        } else {
            next
        };
        let current = current["provider"]
            .as_str()
            .zip(current["model"].as_str())
            .map(|(provider, model)| format!("{provider}/{model}"));
        let display = options
            .iter()
            .map(|(provider, model)| (format!("{provider}/{model}"), None))
            .collect::<Vec<_>>();
        let mut reply = render_model_list(&display, current.as_deref(), "DSH 会话");
        if let Some(failures) = catalog["failures"].as_array() {
            for failure in failures {
                if let (Some(name), Some(message)) =
                    (failure["name"].as_str(), failure["message"].as_str())
                {
                    reply.push_str(&format!("\n{name}: {message}"));
                }
            }
        }
        Ok(PipelineResult::ModelListed {
            instance_id: instance.id.clone(),
            count: options.len(),
            replies: vec![text_reply(reply)],
        })
    }
}
