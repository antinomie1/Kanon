//! Compatibility boundary for older messages that embedded reasoning in text.

/// Splits one complete standard `<think>…</think>` block at the start of legacy text.
/// Without a complete leading block, text stays unchanged; no markup recovery is attempted.
pub fn split_reasoning_tags(text: &str) -> (&str, Option<String>) {
    if let Some(body) = text.trim_start().strip_prefix("<think>")
        && let Some((reasoning, answer)) = body.split_once("</think>")
    {
        (answer.trim_start(), Some(reasoning.to_string()))
    } else {
        (text, None)
    }
}

/// Moves a legacy envelope out of content, preserving an explicit protocol reasoning field.
pub(crate) fn separate(content: &mut Option<String>, reasoning: &mut Option<String>) {
    // An explicit channel is authoritative, even when empty. Its content is an answer, not
    // a second envelope: models may deliberately explain or print reasoning delimiters there.
    if reasoning.is_some() {
        return;
    }
    let Some(text) = content.as_deref() else {
        return;
    };
    let (answer, legacy) = split_reasoning_tags(text);
    if let Some(legacy) = legacy {
        let answer = answer.to_string();
        // A recovered channel also makes repeated normalization idempotent.
        *reasoning = Some(legacy);
        *content = Some(answer);
    }
}

/// Buffers a possible leading legacy block; ordinary/native answer deltas pass through.
#[derive(Default)]
struct ReasoningStream {
    pending: String,
    passthrough: bool,
}

impl ReasoningStream {
    fn push(&mut self, delta: &str, native: bool) -> String {
        if self.passthrough {
            return delta.to_string();
        }
        self.pending.push_str(delta);
        let candidate = self.pending.trim_start();
        if native || (!"<think>".starts_with(candidate) && !candidate.starts_with("<think>")) {
            self.passthrough = true;
            return std::mem::take(&mut self.pending);
        }
        String::new()
    }

    fn finish(&self) -> (String, Option<String>) {
        let (answer, reasoning) = split_reasoning_tags(&self.pending);
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
            chunk.delta_text = filter.push(&chunk.delta_text, has_native_reasoning);
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
        let (answer, reasoning) = filter.finish();
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
