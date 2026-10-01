import type { BotInstanceView, TraceRecord } from './types';

/**
 * What finally happened to one inbound message, as far as the trace stream tells.
 *
 * `open` means no terminal stage was seen: the message may still be in flight, or the reply
 * policy decided not to answer, which the node does not trace. The console therefore never claims
 * that an open message is "thinking".
 */
export type MessageOutcome =
  | 'replied'
  | 'command'
  | 'blocked'
  | 'failed'
  | 'open';

/** One inbound message and what the pipeline did with it. */
export interface TimelineMessage {
  eventId: string;
  platform: string;
  channelId: string;
  senderId: string;
  /** When the message was ingested, in epoch milliseconds. */
  at: number;
  /** When the outcome was reached (reply queued, blocked, command routed); `null` while open. */
  doneAt: number | null;
  outcome: MessageOutcome;
  /** Command name for `command`, the blocking host for `blocked`, the reason for `failed`. */
  detail: string | null;
  sessionId: string | null;
  /** Tool calls made while answering, with the time each one started. */
  tools: { name: string; at: number }[];
}

/**
 * Correlates raw trace records into one entry per inbound message.
 *
 * Stages carry the ingest `event_id`, except tool calls, which only carry the session; those are
 * attached through the session named by `llm_replied` and the time window of the message, so a
 * tool call is never credited to a message that was not being answered at the time.
 */
export function buildMessages(records: TraceRecord[]): TimelineMessage[] {
  const byId = new Map<string, TimelineMessage>();
  const tools: { session: string; name: string; at: number }[] = [];

  for (const record of records) {
    const event = record.event;
    const stage = event.stage;
    const at = record.timestamp_ms;

    if (stage === 'tool_call_started' && event.session_id) {
      tools.push({
        session: event.session_id,
        name: String(event.tool_name ?? ''),
        at,
      });
      continue;
    }

    const id = event.event_id;
    if (!id) continue;

    if (stage === 'ingested') {
      byId.set(id, {
        eventId: id,
        platform: event.platform ?? '',
        channelId: event.channel_id ?? '',
        senderId: event.sender_id ?? '',
        at,
        doneAt: null,
        outcome: 'open',
        detail: null,
        sessionId: null,
        tools: [],
      });
      continue;
    }

    // A stage for a message ingested before this page connected has nothing to attach to.
    const message = byId.get(id);
    if (!message) continue;

    switch (stage) {
      case 'pre_filter_blocked':
        message.outcome = 'blocked';
        message.detail = event.host_id ?? null;
        message.doneAt = at;
        break;
      case 'command_matched':
        message.outcome = 'command';
        message.detail = event.command ?? null;
        break;
      case 'llm_replied':
        message.sessionId = event.session_id ?? null;
        break;
      case 'outbound_queued':
        if (message.outcome === 'open') message.outcome = 'replied';
        message.doneAt = at;
        break;
      case 'circuit_breaker_tripped':
        message.outcome = 'failed';
        message.detail = event.reason ?? null;
        message.doneAt = at;
        break;
    }
  }

  const messages = [...byId.values()];
  for (const message of messages) {
    if (!message.sessionId) continue;
    const end = message.doneAt ?? Number.POSITIVE_INFINITY;
    message.tools = tools
      .filter(
        (tool) =>
          tool.session === message.sessionId &&
          tool.at >= message.at &&
          tool.at <= end,
      )
      .map(({ name, at }) => ({ name, at }));
  }
  return messages;
}

/**
 * The instance answering on each platform.
 *
 * An adapter is claimed by at most one enabled instance, so enabled claims win; a disabled
 * instance is only used when nothing enabled claims the platform (messages that arrived before it
 * was stopped).
 */
export function ownersByPlatform(
  instances: BotInstanceView[],
): Map<string, BotInstanceView> {
  const owners = new Map<string, BotInstanceView>();
  for (const instance of instances) {
    for (const platform of instance.adapters) {
      const current = owners.get(platform);
      if (!current || (!current.enabled && instance.enabled)) {
        owners.set(platform, instance);
      }
    }
  }
  return owners;
}

/** Number of distinct instance colours; see `--k-i0` … `--k-i7` in `app.css`. */
const PALETTE_SIZE = 8;

/**
 * Stable colour slot of an instance, derived from its id so it stays the same across reloads and
 * browsers without being stored anywhere.
 */
export function colorSlot(id: string): number {
  let hash = 0;
  for (let i = 0; i < id.length; i++) {
    hash = (hash * 31 + id.charCodeAt(i)) | 0;
  }
  return Math.abs(hash) % PALETTE_SIZE;
}
