//! Cache-safe context compaction.
//!
//! # The problem with trimming
//! A model's context window is finite, so a long conversation has to shrink eventually. Dropping
//! the oldest message on every turn (a sliding window) keeps the size bounded but rewrites the
//! prompt's prefix each time, so the provider's cache never hits and every turn is billed as a
//! first-time read of the whole history.
//!
//! # What this module does instead
//! History only grows until it reaches a fraction of the model's context window. Then it is
//! compacted **once**, in two steps that both lean on the cache:
//!
//! 1. **Summarize with the same prefix.** The summarization request is the conversation's own
//!    request — same tools, same system block, same history — with one extra user message asking
//!    for a summary appended at the very end. The provider reads everything before that message from
//!    the cache it warmed a moment ago, so the summary costs little more than its own output. It
//!    runs right after a turn's reply has been produced, which is when that cache is hottest, and in
//!    the background so the user never waits for it.
//! 2. **Mount the summary in the next prefix.** The summarized messages are replaced by the summary,
//!    which goes into the static system block (see [`crate::layout`]). The prompt prefix changes at
//!    that one moment — a single cache miss — and is then stable and append-only again until the
//!    next compaction.
//!
//! Between compactions nothing about the prefix changes, no matter how many turns pass.

use crate::gateway::types::{ChatMessage, Role};

/// The message appended to a conversation to ask for its summary.
///
/// It comes last on purpose: everything before it is byte-identical to the previous request, which
/// is what lets the provider serve the whole conversation from its cache.
pub const COMPACTION_INSTRUCTION: &str = "The conversation above is about to be compacted to save \
context space. Do not call any tools. Write a summary that lets you continue this conversation \
seamlessly: the user's goals and preferences, names and facts mentioned, decisions and conclusions \
reached, the results of tool calls that still matter, and anything left unfinished. Be concise and \
factual. Reply with the summary only, in plain text, with no preamble.";

/// Heading of the summary inside the system block.
pub const SUMMARY_HEADING: &str = "## Conversation summary";

/// When a conversation is compacted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactionPolicy {
    /// Fraction of the model's context window at which compaction starts (`0.0 < ratio <= 1.0`).
    ///
    /// Below 1.0 on purpose: the window must still hold the reply and the next user message.
    pub trigger_ratio: f32,
    /// Context window assumed when the model's is unknown, in tokens.
    ///
    /// Deliberately modest: an unknown model is far likelier to be small than to be huge, and
    /// compacting a little early costs one summary, while overflowing costs a failed turn.
    pub default_context_tokens: u32,
    /// A history shorter than this many messages is never compacted.
    pub min_messages: usize,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            trigger_ratio: 0.7,
            default_context_tokens: 32_768,
            min_messages: 4,
        }
    }
}

impl CompactionPolicy {
    /// Checks the tunables an operator could get wrong.
    pub fn validate(&self) -> Result<(), String> {
        if !(self.trigger_ratio > 0.0 && self.trigger_ratio <= 1.0) {
            return Err(format!(
                "compaction trigger ratio {} must be greater than 0 and at most 1",
                self.trigger_ratio
            ));
        }
        if self.default_context_tokens == 0 {
            return Err("compaction default context window must not be zero".to_string());
        }
        Ok(())
    }

    /// Context size, in tokens, at which compaction starts for a model with this window.
    pub fn trigger_tokens(&self, context_length: Option<u32>) -> usize {
        let window = context_length
            .filter(|length| *length > 0)
            .unwrap_or(self.default_context_tokens);
        // `f32` cannot represent 0.7 exactly, so round instead of truncating: 70% of 100,000 must
        // be 70,000, not 69,999.
        (f64::from(window) * f64::from(self.trigger_ratio)).round() as usize
    }

    /// Whether a context of `context_tokens` tokens is large enough to compact.
    pub fn is_exceeded(&self, context_tokens: usize, context_length: Option<u32>) -> bool {
        context_tokens >= self.trigger_tokens(context_length)
    }
}

/// The system-block text that carries a summary into the next requests.
pub fn summary_block(summary: &str) -> String {
    format!(
        "{SUMMARY_HEADING}\nThe earlier part of this conversation was compacted into the summary \
         below.\n\n{}",
        summary.trim()
    )
}

/// Whether a history is in a state that can be summarized and dropped.
///
/// It must end with a finished assistant reply. A history ending in a user message is a turn still
/// waiting for its answer, and one ending in a tool call or result is mid tool-loop: folding either
/// away would orphan what follows it.
pub fn ends_cleanly(history: &[ChatMessage]) -> bool {
    history.last().is_some_and(|message| {
        message.role == Role::Assistant
            && message
                .tool_calls
                .as_ref()
                .is_none_or(|calls| calls.is_empty())
    })
}
