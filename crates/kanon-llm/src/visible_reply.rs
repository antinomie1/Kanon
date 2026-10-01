//! Final boundary between a model answer and a chat platform.
//!
//! Protocol reasoning and structured tool calls already travel in their own channels. This module
//! catches what still slips into answer text — a `</think>` whose opening tag was injected by the
//! chat template, a reasoning block in the middle of the answer, or tool-call markup the agent
//! could not parse — so none of it is ever delivered as part of a reply.

/// Splits a final answer into the user-visible text and any reasoning found inside it.
///
/// The returned reasoning is plain content without tag markup. Tool-call markup is dropped
/// entirely: it is neither an answer nor reasoning.
pub fn visible_reply(text: &str) -> (String, Option<String>) {
    let mut reasoning = Vec::new();
    let (answer, leading) = crate::gateway::reasoning::split_reasoning_tags(text);
    push_nonblank(&mut reasoning, leading.as_deref().unwrap_or_default());
    let mut answer = answer.to_string();

    // Some templates open the reasoning block in the prompt, so the completion carries only
    // the closing tag: everything before it is reasoning.
    let lower = answer.to_ascii_lowercase();
    if let Some(close) = lower.find("</think>")
        && find_open(&lower[..close], "<think").is_none()
    {
        push_nonblank(&mut reasoning, &answer[..close]);
        answer = answer[close + "</think>".len()..].to_string();
    }

    let answer = take_blocks(&answer, "<think", "</think>", Some(&mut reasoning));
    let answer = take_blocks(&answer, "<tool_calls", "</tool_calls>", None);
    let answer = take_blocks(&answer, "<tool_call", "</tool_call>", None);
    let answer = take_blocks(&answer, "<function=", "</function>", None);

    let reasoning = (!reasoning.is_empty()).then(|| reasoning.join("\n\n"));
    (answer.trim().to_string(), reasoning)
}

/// Appends trimmed text unless it is blank.
fn push_nonblank(sink: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        sink.push(text.to_string());
    }
}

/// Removes every `open … close` block, case-insensitively, optionally collecting the bodies.
///
/// An unclosed block runs to the end of the text: a truncated block is still markup, and
/// delivering its tail would leak exactly what this boundary exists to hide.
fn take_blocks(text: &str, open: &str, close: &str, mut sink: Option<&mut Vec<String>>) -> String {
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
        let (body_end, next) = match lower[body_start..].find(close) {
            Some(offset) => (body_start + offset, body_start + offset + close.len()),
            None => (text.len(), text.len()),
        };
        if let Some(sink) = sink.as_deref_mut() {
            push_nonblank(sink, &text[body_start..body_end]);
        }
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
