//! Instance input normalization and identifier allocation.

use super::*;

/// Normalizes and validates a submitted instance name.
pub(super) fn normalize_name(raw: &str) -> Result<String, InstanceError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(InstanceError::Invalid("name must not be empty".to_string()));
    }
    if name.chars().count() > 64 {
        return Err(InstanceError::Invalid(
            "name must be at most 64 characters".to_string(),
        ));
    }
    Ok(name.to_string())
}

/// Builds the persisted record from a draft, normalizing every optional field.
pub(super) fn build_instance(
    id: String,
    name: String,
    draft: InstanceDraft,
) -> Result<BotInstance, InstanceError> {
    prepare_instance(BotInstance {
        id,
        name,
        enabled: draft.enabled,
        conversation_mode: draft.conversation_mode,
        conversation_rules: draft
            .conversation_rules
            .unwrap_or(draft.conversation_mode == ConversationMode::Simulation),
        simulation: draft.simulation,
        adapters: draft.adapters,
        persona_id: draft.persona_id,
        system_prompt: draft.system_prompt,
        agent: draft.agent,
        model: draft.model,
        reply_policy: draft.reply_policy,
        context_policy: draft.context_policy,
        session_scope: draft.session_scope,
        observe_group: draft.observe_group,
        command_policy: draft.command_policy,
        bash: draft.bash,
        plugins: draft.plugins,
        skills: draft.skills,
        mcp: draft.mcp,
        session_generations: HashMap::new(),
    })
}

/// Normalizes and validates API candidates and restored records without touching session state.
pub(super) fn prepare_instance(mut instance: BotInstance) -> Result<BotInstance, InstanceError> {
    if instance.id.is_empty()
        || !instance
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(InstanceError::Invalid(format!(
            "invalid instance id '{}'",
            instance.id
        )));
    }
    instance.name = normalize_name(&instance.name)?;
    instance
        .simulation
        .validate()
        .map_err(InstanceError::Invalid)?;
    let mut adapters: Vec<String> = Vec::new();
    for adapter in std::mem::take(&mut instance.adapters) {
        let platform = adapter.trim();
        if platform.is_empty() {
            return Err(InstanceError::Invalid(
                "adapter identifiers must not be empty".to_string(),
            ));
        }
        if !adapters.iter().any(|existing| existing == platform) {
            adapters.push(platform.to_string());
        }
    }

    instance.agent = instance
        .agent
        .map(|agent| agent.trim().to_string())
        .filter(|agent| !agent.is_empty());
    if let Some(agent) = instance.agent.as_deref() {
        kanon_llm::check_agent_id(agent).map_err(InstanceError::Invalid)?;
    }

    instance.model = normalize_model(instance.model)?;

    instance.system_prompt = instance
        .system_prompt
        .map(|prompt| prompt.trim().to_string())
        .filter(|prompt| !prompt.is_empty());

    instance.persona_id = instance
        .persona_id
        .map(|persona| persona.trim().to_string())
        .filter(|persona| !persona.is_empty());

    if let Some(policy) = instance.reply_policy.as_ref() {
        policy.validate().map_err(InstanceError::Invalid)?;
    }
    // Normalized like the node-wide policy, so a stored override matches exactly what is enforced.
    instance.command_policy = instance
        .command_policy
        .map(CommandPolicy::prepare)
        .transpose()
        .map_err(InstanceError::Invalid)?;

    instance.adapters = adapters;
    Ok(instance)
}

/// Canonicalizes an optional override without ever guessing a provider for a bare model id.
pub(super) fn normalize_model(model: Option<String>) -> Result<Option<String>, InstanceError> {
    model
        .filter(|model| !model.trim().is_empty())
        .map(|model| {
            let reference = kanon_llm::ModelRef::parse(&model);
            if reference.provider().is_none() {
                return Err(InstanceError::Invalid(format!(
                    "model '{model}' must be written as <provider>/<model-id>"
                )));
            }
            Ok(reference.canonical())
        })
        .transpose()
}

/// Derives a unique, readable identifier from an instance name.
pub(super) fn unique_id(instances: &HashMap<String, BotInstance>, name: &str) -> String {
    let mut slug: String = name
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-').to_string();
    // Non-ASCII names (e.g. 黑猪AI) slugify to nothing, so fall back to a stable prefix.
    let base = if slug.is_empty() {
        "bot".to_string()
    } else {
        slug
    };

    if !instances.contains_key(&base) {
        return base;
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base}-{suffix}");
        if !instances.contains_key(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}
