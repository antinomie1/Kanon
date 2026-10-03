import type { ChatCompletionRequest } from '../types';

export interface StreamCallbacks {
  onChunk: (delta: string, reasoning?: string) => void;
  onFinish?: (reason?: string) => void;
  onError?: (err: Error) => void;
}

/** Reads the gateway's delta/done/error protocol; only an explicit terminal event is success. */
export async function streamChatCompletion(
  req: ChatCompletionRequest,
  callbacks: StreamCallbacks,
  signal?: AbortSignal,
): Promise<void> {
  let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
  try {
    const res = await fetch('/api/v1/chat/completions', {
      method: 'POST',
      headers: {
        'Content-Type': 'application/json',
        Accept: 'text/event-stream',
      },
      body: JSON.stringify({ ...req, stream: true }),
      signal,
    });

    if (!res.ok) {
      const errText = await res.text();
      throw new Error(`HTTP ${res.status}: ${errText}`);
    }

    reader = res.body?.getReader();
    if (!reader) {
      throw new Error('ReadableStream not supported on response body');
    }

    const decoder = new TextDecoder();
    let buffer = '';

    while (true) {
      const { done, value } = await reader.read();
      if (done) break;

      buffer += decoder.decode(value, { stream: true });
      const lines = buffer.split('\n');
      buffer = lines.pop() ?? '';

      for (const line of lines) {
        const trimmed = line.trim();
        if (!trimmed || trimmed.startsWith(':')) continue;

        if (trimmed.startsWith('data:')) {
          const payload = trimmed.slice(5).trim();
          if (payload === '[DONE]') {
            callbacks.onFinish?.('stop');
            return;
          }

          const data = JSON.parse(payload);
          if (data.type === 'error') {
            throw new Error(data.message || 'Chat completion failed');
          }
          if (data.type !== 'delta' && data.type !== 'done') {
            throw new Error('Unexpected chat stream event');
          }
          if (data.delta !== undefined || data.reasoning !== undefined) {
            callbacks.onChunk(data.delta ?? '', data.reasoning ?? undefined);
          }
          if (data.type === 'done') {
            callbacks.onFinish?.(data.finish_reason ?? 'stop');
            return;
          }
        }
      }
    }

    throw new Error('Chat stream ended before a completion event');
  } catch (err) {
    if (signal?.aborted) return;
    callbacks.onError?.(err instanceof Error ? err : new Error(String(err)));
  } finally {
    reader?.releaseLock();
  }
}
