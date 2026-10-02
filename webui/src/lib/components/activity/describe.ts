import { t } from '../../stores/i18n.svelte';
import type { PipelineEventPayload } from '../../types';

/** How a stage reads at a glance: neutral progress, a good outcome, a snag, or a failure. */
export type Tone = 'idle' | 'ok' | 'warn' | 'bad';

/** One trace record put into words: a short title and the facts worth showing under it. */
export interface Described {
  title: string;
  tone: Tone;
  details: string[];
}

/**
 * Filter groups of the activity list, as understood by `pipelineStore.selectedStage`. The node
 * publishes finer stages than a person wants to pick between, so related ones share a group.
 */
export const STAGE_GROUPS = [
  'ingested',
  'pre_filter',
  'command',
  'llm',
  'tool',
  'outbound',
  'breaker',
] as const;
export type StageGroup = (typeof STAGE_GROUPS)[number];

function text(value: unknown): string | null {
  return typeof value === 'string' && value !== '' ? value : null;
}

function count(value: unknown): number | null {
  return typeof value === 'number' ? value : null;
}

/** Where a message came from or went to, as `platform, channel`. */
function place(event: PipelineEventPayload): string | null {
  const platform = text(event.platform);
  const channel = text(event.channel_id);
  if (platform && channel) return t('activity.place', { platform, channel });
  return platform ?? channel;
}

/** Title of a `no_reply` record by its cause; an unknown cause keeps the generic title. */
function noReplyTitle(cause: unknown): string {
  switch (cause) {
    case 'reply_policy':
      return t('activity.s_no_reply_policy');
    case 'notice':
      return t('activity.s_no_reply_notice');
    case 'no_instance':
      return t('activity.s_no_reply_instance');
    case 'nothing_to_say':
      return t('activity.s_no_reply_empty');
    default:
      return t('activity.s_no_reply');
  }
}

/**
 * Turns one trace record into words.
 *
 * Unknown stages fall through to their raw name, so a stage added to the node later still shows
 * up rather than vanishing from the list.
 */
export function describe(event: PipelineEventPayload): Described {
  const stage = event.stage;
  const details: string[] = [];
  const add = (value: string | null) => {
    if (value) details.push(value);
  };

  switch (stage) {
    case 'ingested':
      add(place(event));
      add(
        text(event.sender_id) &&
          t('activity.from', { sender: event.sender_id as string }),
      );
      return { title: t('activity.s_ingested'), tone: 'idle', details };
    case 'pre_filter_started':
      add(
        count(event.host_count) !== null
          ? t('activity.hosts', { n: event.host_count as number })
          : null,
      );
      return {
        title: t('activity.s_pre_filter_started'),
        tone: 'idle',
        details,
      };
    case 'pre_filter_passed':
      return {
        title: t('activity.s_pre_filter_passed'),
        tone: 'idle',
        details,
      };
    case 'pre_filter_blocked':
      add(text(event.host_id));
      return {
        title: t('activity.s_pre_filter_blocked'),
        tone: 'warn',
        details,
      };
    case 'command_matched':
      add(
        text(event.plugin_id) &&
          t('activity.by_plugin', { plugin: event.plugin_id as string }),
      );
      return {
        title: t('activity.s_command_matched', {
          command: String(event.command ?? ''),
        }),
        tone: 'idle',
        details,
      };
    case 'command_not_found':
      return {
        title: t('activity.s_command_not_found', {
          command: String(event.command ?? ''),
        }),
        tone: 'warn',
        details,
      };
    case 'llm_request':
      add(text(event.model));
      add(
        count(event.message_count) !== null
          ? t('activity.messages', { n: event.message_count as number })
          : null,
      );
      add(
        count(event.tool_count)
          ? t('activity.tools', { n: event.tool_count as number })
          : null,
      );
      return { title: t('activity.s_llm_request'), tone: 'idle', details };
    case 'llm_response': {
      add(
        count(event.content_length) !== null
          ? t('activity.chars', { n: event.content_length as number })
          : null,
      );
      const prompt = count(event.prompt_tokens);
      if (prompt) {
        const cached = count(event.cached_tokens) ?? 0;
        add(
          cached > 0
            ? t('activity.tokens_cached', { n: prompt, cached })
            : t('activity.tokens', { n: prompt }),
        );
      }
      if (event.requested_tools === true) add(t('activity.wants_tools'));
      add(text(event.finish_reason));
      return { title: t('activity.s_llm_response'), tone: 'idle', details };
    }
    case 'llm_replied':
      add(
        count(event.content_length) !== null
          ? t('activity.chars', { n: event.content_length as number })
          : null,
      );
      return { title: t('activity.s_llm_replied'), tone: 'idle', details };
    case 'no_reply': {
      // The node closes every trace it does not answer, so the cause names a deliberate decision
      // rather than leaving the list to look like a stalled pipeline.
      add(text(event.reason));
      return { title: noReplyTitle(event.cause), tone: 'idle', details };
    }
    case 'tool_call_started':
      return {
        title: t('activity.s_tool_started', {
          tool: String(event.tool_name ?? ''),
        }),
        tone: 'idle',
        details,
      };
    case 'tool_call_finished':
      return event.success === false
        ? {
            title: t('activity.s_tool_failed', {
              tool: String(event.tool_name ?? ''),
            }),
            tone: 'bad',
            details,
          }
        : {
            title: t('activity.s_tool_finished', {
              tool: String(event.tool_name ?? ''),
            }),
            tone: 'ok',
            details,
          };
    case 'tool_call_denied':
      return {
        title: t('activity.s_tool_denied', {
          tool: String(event.tool_name ?? ''),
        }),
        tone: 'warn',
        details,
      };
    case 'outbound_queued':
      add(place(event));
      add(
        count(event.segment_count) !== null
          ? t('activity.segments', { n: event.segment_count as number })
          : null,
      );
      return { title: t('activity.s_outbound_queued'), tone: 'idle', details };
    case 'outbound_delivered':
      add(place(event));
      return { title: t('activity.s_outbound_delivered'), tone: 'ok', details };
    case 'outbound_failed':
      add(place(event));
      add(text(event.reason));
      return { title: t('activity.s_outbound_failed'), tone: 'bad', details };
    case 'circuit_breaker_tripped':
      add(text(event.host_id));
      add(text(event.reason));
      return { title: t('activity.s_breaker'), tone: 'bad', details };
    case 'session_reset':
      return { title: t('activity.s_session_reset'), tone: 'idle', details };
    case 'persona_switched':
      add(text(event.persona_id));
      return { title: t('activity.s_persona_switched'), tone: 'idle', details };
    case 'plugin_config_updated':
      add(text(event.plugin_id));
      return { title: t('activity.s_plugin_config'), tone: 'idle', details };
    case 'plugin_restarted':
      add(text(event.host_id));
      return { title: t('activity.s_plugin_restarted'), tone: 'idle', details };
    case 'plugin_installed':
      add(text(event.plugin_id));
      return { title: t('activity.s_plugin_installed'), tone: 'ok', details };
    default:
      return { title: stage, tone: 'idle', details };
  }
}
