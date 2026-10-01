//! Final boundary between a normalized answer and platform delivery.
//!
//! Reasoning is separated at the provider/agent boundary, and tool-call markup the agent could
//! parse was already turned into real calls and cut out there (`tool_call_text`). Whatever markup
//! is still in the answer channel was deliberately left as text — a documentation example, a code
//! sample, or a malformed block — so reclassifying it here would corrupt technical answers and
//! could empty a reply that consists only of such markup.

/// Returns the answer text to deliver.
///
/// Think tags and tool-call markup in this already normalized channel are content and are
/// preserved; only surrounding whitespace is trimmed.
pub fn visible_reply(text: &str) -> String {
    text.trim().to_string()
}
