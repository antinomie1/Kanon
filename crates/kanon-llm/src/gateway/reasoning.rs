//! Compatibility boundary for older messages that embedded reasoning in text.

/// Splits only the leading, unquoted legacy reasoning envelope from the answer.
///
/// Consecutive and nested blocks are consumed together. An unfinished envelope has no
/// visible answer. Inline mentions, Markdown code and text after the envelope stay verbatim.
/// A bare leading `<think>` is reserved for the legacy envelope; literal examples must quote
/// or fence it, since an unquoted example is indistinguishable from an old reasoning block.
pub fn split_reasoning_tags(text: &str) -> (&str, Option<String>) {
    let mut rest = text;
    let mut reasoning = Vec::new();
    loop {
        let candidate = rest.trim_start();
        let Some(open_len) = tag_len(candidate, false) else {
            if is_partial_open(candidate) {
                return ("", Some(reasoning.join("\n\n")));
            }
            return if reasoning.is_empty() {
                (text, None)
            } else {
                (candidate, Some(reasoning.join("\n\n")))
            };
        };
        let body = &candidate[open_len..];
        let mut offset = 0;
        let mut depth = 1usize;
        let mut end = None;
        while let Some(relative) = body[offset..].find('<') {
            let start = offset + relative;
            let tail = &body[start..];
            if let Some(len) = tag_len(tail, true) {
                depth -= 1;
                if depth == 0 {
                    end = Some((start, start + len));
                    break;
                }
                offset = start + len;
            } else if let Some(len) = tag_len(tail, false) {
                depth += 1;
                offset = start + len;
            } else {
                offset = start + 1;
            }
        }
        match end {
            Some((close_start, close_end)) => {
                reasoning.push(body[..close_start].to_string());
                rest = &body[close_end..];
            }
            None => {
                reasoning.push(body.to_string());
                return ("", Some(reasoning.join("\n\n")));
            }
        }
    }
}

/// Length of a complete case-insensitive tag, accepting whitespace before `>`.
fn tag_len(text: &str, closing: bool) -> Option<usize> {
    let prefix = if closing { "</think" } else { "<think" };
    if !text.get(..prefix.len())?.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let tail = &text[prefix.len()..];
    let trimmed = tail.trim_start();
    trimmed
        .starts_with('>')
        .then_some(text.len() - trimmed.len() + 1)
}

/// Whether a stream may still complete a leading opening tag.
fn is_partial_open(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    const PREFIX: &str = "<think";
    if text.len() <= PREFIX.len() {
        return PREFIX[..text.len()].eq_ignore_ascii_case(text);
    }
    text.get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
        && text[PREFIX.len()..].trim().is_empty()
}

/// Moves a legacy envelope out of content, preserving an explicit protocol reasoning field.
pub(crate) fn separate(content: &mut Option<String>, reasoning: &mut Option<String>) {
    let Some(text) = content.as_deref() else {
        return;
    };
    let (answer, legacy) = split_reasoning_tags(text);
    if let Some(legacy) = legacy {
        let answer = answer.to_string();
        // Native protocol reasoning must round-trip verbatim. Only old messages lacking that
        // field recover it from the display envelope; echoed envelopes still leave the answer.
        if reasoning.is_none() && !legacy.is_empty() {
            *reasoning = Some(legacy);
        }
        *content = Some(answer);
    }
}

/// Incremental boundary that buffers only possible legacy envelopes.
///
/// Native reasoning deltas already have their own channel. Ordinary answers pass immediately;
/// a leading legacy envelope is held until completion so split tags and truncated/nested blocks
/// cannot escape in earlier chunks. This also avoids rescanning a growing reasoning prefix.
#[derive(Default)]
struct ReasoningStream {
    pending: String,
    passthrough: bool,
    legacy: bool,
}

impl ReasoningStream {
    fn push(&mut self, delta: &str) -> String {
        if self.passthrough {
            return delta.to_string();
        }
        self.pending.push_str(delta);
        if !self.legacy {
            let candidate = self.pending.trim_start();
            self.legacy = tag_len(candidate, false).is_some();
            if !self.legacy && !candidate.is_empty() && !is_partial_open(candidate) {
                self.passthrough = true;
                return std::mem::take(&mut self.pending);
            }
        }
        String::new()
    }

    fn finish(&mut self) -> (String, Option<String>) {
        let text = std::mem::take(&mut self.pending);
        let (answer, reasoning) = split_reasoning_tags(&text);
        (answer.to_string(), reasoning)
    }
}

/// Applies the legacy boundary to a provider stream without mixing its native channels.
pub(crate) fn separate_stream(mut stream: super::ChatChunkStream) -> super::ChatChunkStream {
    use tokio_stream::StreamExt;
    let (tx, rx) = tokio::sync::mpsc::channel(32);
    tokio::spawn(async move {
        let mut filter = ReasoningStream::default();
        let mut finish_reason = None;
        let mut has_native_reasoning = false;
        while let Some(result) = stream.next().await {
            let mut chunk = match result {
                Ok(chunk) => chunk,
                Err(error) => {
                    let _ = tx.send(Err(error)).await;
                    return;
                }
            };
            has_native_reasoning |= chunk.reasoning_text.is_some();
            chunk.delta_text = filter.push(&chunk.delta_text);
            let finished = chunk.is_finished;
            if finished {
                finish_reason = chunk.finish_reason.take();
                chunk.is_finished = false;
            }
            if (!chunk.delta_text.is_empty()
                || chunk.reasoning_text.is_some()
                || !chunk.tool_calls.is_empty())
                && tx.send(Ok(chunk)).await.is_err()
            {
                return;
            }
            if finished {
                break;
            }
        }
        let (answer, mut reasoning) = filter.finish();
        if has_native_reasoning {
            reasoning = None;
        }
        if !answer.is_empty() || reasoning.is_some() {
            let _ = tx
                .send(Ok(super::ChatChunk {
                    delta_text: answer,
                    reasoning_text: reasoning,
                    ..super::ChatChunk::default()
                }))
                .await;
        }
        let _ = tx.send(Ok(super::ChatChunk::done(finish_reason))).await;
    });
    Box::pin(tokio_stream::wrappers::ReceiverStream::new(rx))
}
