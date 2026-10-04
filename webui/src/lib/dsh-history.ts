import type { DshRecord } from './types';

/** One native message shown in the console, with the original blocks retained for media. */
export interface DshMessage {
  seq: number;
  role: 'user' | 'assistant';
  text: string;
  reasoning: string;
  blocks: Array<Record<string, unknown>>;
}

/** Console IDs retain only the routing target; all durable session state stays in DSH. */
export function dshConsoleTarget(id: string): string | null {
  const match = /^kanon-console-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}-(.*)$/.exec(id);
  if (!match) return null;
  try {
    const target = decodeURIComponent(match[1]);
    return target === '' || target.startsWith('i:') ? target : null;
  } catch { return null; }
}

/** Reads DSH's human transcript; replacement copies belong only to the model's context. */
export function dshHistory(records: DshRecord[]): DshMessage[] {
  const visible = new Map<number, DshMessage>();
  for (const { event } of records) {
    const op = event.surfaceOp;
    if (op && op !== 'append') continue;
    if (event.type !== 'user/message' && event.type !== 'assistant/message') continue;
    const role = event.type === 'user/message' ? 'user' : 'assistant';
    const payload = role === 'user' ? event.data : event.data.message as Record<string, unknown>;
    if (!payload || !Array.isArray(payload.content)) throw new Error('Invalid DSH message');
    const blocks = payload.content as Array<Record<string, unknown>>;
    visible.set(event.seq, {
      seq: event.seq, role, blocks,
      text: blocks.filter(block => block.type === 'text').map(block => String(block.text ?? '')).join(''),
      reasoning: blocks.filter(block => block.type === 'reasoning').map(block => String(block.text ?? '')).join(''),
    });
  }
  return [...visible.values()];
}
