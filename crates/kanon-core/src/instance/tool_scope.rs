//! Instance policy lookup for runtimes whose durable session ids do not encode a Kanon route.

#[cfg(feature = "dsh")]
tokio::task_local! {
    static TOOL_INSTANCE: Option<String>;
}

/// Carries trusted admission identity across native tools and lifecycle hooks.
#[cfg(feature = "dsh")]
pub(crate) async fn with_tool_instance<F: std::future::Future>(
    instance: Option<String>,
    future: F,
) -> F::Output {
    TOOL_INSTANCE.scope(instance, future).await
}

/// Uses explicit remote routing when present, otherwise the existing builtin namespace.
pub(crate) fn tool_instance(session_id: &str) -> Option<String> {
    #[cfg(feature = "dsh")]
    if let Ok(instance) = TOOL_INSTANCE.try_with(Clone::clone) {
        return instance;
    }
    super::BotInstance::instance_id_from_session(session_id).map(str::to_owned)
}
