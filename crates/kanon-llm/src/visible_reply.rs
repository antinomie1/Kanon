//! Removes unparsed tool-call markup from a normalized answer before platform delivery.
//!
//! Reasoning is separated at the provider/agent boundary. Once text is in the answer channel,
//! think tags are ordinary content; reclassifying them here would corrupt examples and code.

/// Returns answer text with unparsed tool-call markup removed.
/// Reasoning delimiters in this already normalized channel are preserved.
pub fn visible_reply(text: &str) -> String {
    let answer = take_blocks(text, "<tool_calls", "</tool_calls>");
    let answer = take_blocks(&answer, "<tool_call", "</tool_call>");
    let answer = take_blocks(&answer, "<function=", "</function>");
    answer.trim().to_string()
}

/// Removes every `open … close` block, case-insensitively.
///
/// An unclosed block runs to the end of the text: a truncated block is still markup, and
/// delivering its tail would leak exactly what this boundary exists to hide.
fn take_blocks(text: &str, open: &str, close: &str) -> String {
    // ASCII lowercasing keeps byte offsets identical, so indices found here slice `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = find_open(&lower[cursor..], open) {
        let start = cursor + relative;
        out.push_str(&text[cursor..start]);
        let body_start = lower[start..]
            .find('>')
            .map_or(text.len(), |offset| start + offset + 1);
        let next = match lower[body_start..].find(close) {
            Some(offset) => body_start + offset + close.len(),
            None => text.len(),
        };
        cursor = next;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Finds an opening tag, rejecting longer names that merely share its prefix (`<tool_calls`
/// is not `<tool_call`, `<thinker` is not `<think`).
fn find_open(lower: &str, open: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(relative) = lower[from..].find(open) {
        let start = from + relative;
        let next = lower[start + open.len()..].chars().next();
        if open.ends_with('=') || next.is_none_or(|c| c == '>' || c.is_whitespace()) {
            return Some(start);
        }
        from = start + open.len();
    }
    None
}
