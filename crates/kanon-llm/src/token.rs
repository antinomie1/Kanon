//! Lightweight Token Budget Estimator.
//!
//! Provides sub-microsecond token estimation without pulling in heavy BPE tokenizers
//! or downloading large vocabulary files. Accurately accounts for English text,
//! CJK (Chinese, Japanese, Korean) characters, message envelope framing, and tool calls.

use crate::gateway::types::{ChatMessage, ToolCall};

/// Token cost attributed to one image part.
///
/// Providers bill an image as a flat block whose size depends on its resolution, which the
/// estimator cannot know; a constant keeps the budget from ignoring media entirely.
const IMAGE_PART_TOKENS: usize = 256;

/// Estimates the token count of a plain string slice.
///
/// Heuristic:
/// - CJK characters and full-width symbols count as ~1 token each;
/// - Standard ASCII words and punctuation average ~4 characters per token;
/// - Minimum of 1 token for non-empty text.
pub fn estimate_text_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }

    let mut cjk_count = 0usize;
    let mut ascii_chars = 0usize;

    for ch in text.chars() {
        if is_cjk(ch) {
            cjk_count += 1;
        } else {
            ascii_chars += 1;
        }
    }

    // ASCII tokenization approximation: ~4 characters per token
    let ascii_tokens = ascii_chars.div_ceil(4);
    (cjk_count + ascii_tokens).max(1)
}

/// Backward-compatible alias for [`estimate_text_tokens`].
pub use estimate_text_tokens as estimate_tokens;

/// Estimates the token cost of an individual conversational message,
/// including role framing overhead (~4 tokens) and optional tool calls.
pub fn estimate_message_tokens(msg: &ChatMessage) -> usize {
    // Standard OpenAI/Anthropic message framing overhead (~4 tokens per message)
    let mut total = 4usize;

    if let Some(ref content) = msg.content {
        total += estimate_text_tokens(content);
    }

    // Multimodal parts are additional payload: providers transmit both the textual projection in
    // `content` and the parts, so both are counted.
    if let Some(ref parts) = msg.parts {
        for part in parts {
            total += match part {
                crate::gateway::types::ContentPart::Text { text } => estimate_text_tokens(text),
                crate::gateway::types::ContentPart::Image { .. } => IMAGE_PART_TOKENS,
            };
        }
    }

    if let Some(ref tool_calls) = msg.tool_calls {
        for call in tool_calls {
            total += estimate_tool_call_tokens(call);
        }
    }

    if let Some(ref tool_call_id) = msg.tool_call_id {
        total += estimate_text_tokens(tool_call_id);
    }

    if let Some(ref name) = msg.name {
        total += estimate_text_tokens(name);
    }

    total
}

/// Estimates tokens for a tool call request (function name + JSON arguments).
pub fn estimate_tool_call_tokens(call: &ToolCall) -> usize {
    let name_tokens = estimate_text_tokens(&call.name);
    let args_str = call.arguments.to_string();
    let args_tokens = estimate_text_tokens(&args_str);
    // Overhead for function envelope and argument syntax
    3 + name_tokens + args_tokens
}

/// Estimates the total token count of an entire conversation history.
pub fn estimate_conversation_tokens(messages: &[ChatMessage]) -> usize {
    messages.iter().map(estimate_message_tokens).sum::<usize>() + 2 // Conversation priming tokens
}

/// Helper identifying CJK, Hiragana, Katakana, and Hangul unicode ranges.
#[inline]
fn is_cjk(c: char) -> bool {
    matches!(
        c,
        '\u{4e00}'..='\u{9fff}'   // CJK Unified Ideographs
        | '\u{3400}'..='\u{4dbf}' // CJK Unified Ideographs Extension A
        | '\u{20000}'..='\u{2a6df}' // CJK Extension B
        | '\u{f900}'..='\u{faff}' // CJK Compatibility Ideographs
        | '\u{3040}'..='\u{309f}' // Hiragana
        | '\u{30a0}'..='\u{30ff}' // Katakana
        | '\u{ac00}'..='\u{d7af}' // Hangul Syllables
        | '\u{3000}'..='\u{303f}' // CJK Symbols and Punctuation
        | '\u{ff00}'..='\u{ffef}' // Halfwidth and Fullwidth Forms
    )
}
