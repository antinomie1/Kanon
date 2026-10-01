<script lang="ts">
import { RefreshCw } from 'lucide-svelte';
import { i18n, t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { pipelineStore } from '../../stores/pipeline.svelte';
import { router } from '../../stores/router.svelte';
import {
  buildMessages,
  colorSlot,
  ownersByPlatform,
  type TimelineMessage,
} from '../../timeline';
import Button from '../ui/Button.svelte';
import Seg from '../ui/Seg.svelte';

/** Visible time span of the timeline. */
type Span = '1m' | '5m' | '1h';

let {
  span = $bindable('5m'),
  messages = $bindable([]),
}: {
  span?: Span;
  /** Messages inside the visible span, handed back so the page can summarise them. */
  messages?: TimelineMessage[];
} = $props();

const SPAN_MS: Record<Span, number> = {
  '1m': 60_000,
  '5m': 300_000,
  '1h': 3_600_000,
};
// Refresh often enough that a note moves by about a pixel per tick on a wide screen.
const TICK_MS: Record<Span, number> = { '1m': 1000, '5m': 2000, '1h': 15000 };

let now = $state(Date.now());
$effect(() => {
  const timer = window.setInterval(() => (now = Date.now()), TICK_MS[span]);
  now = Date.now();
  return () => window.clearInterval(timer);
});

const windowMs = $derived(SPAN_MS[span]);
const start = $derived(now - windowMs);

const all = $derived(buildMessages(pipelineStore.records));
const visible = $derived(
  all.filter((message) => (message.doneAt ?? message.at) >= start),
);
$effect(() => {
  messages = visible;
});

const owners = $derived(ownersByPlatform(instancesStore.instances));

interface Lane {
  key: string;
  name: string;
  slot: number | null;
  enabled: boolean;
  messages: TimelineMessage[];
}

// One lane per instance: enabled ones always, stopped ones only while they still have visible
// messages. Messages from platforms no instance claims get a lane of their own, so nothing that
// arrived is hidden.
const lanes = $derived.by<Lane[]>(() => {
  const map = new Map<string, Lane>();
  for (const instance of instancesStore.instances) {
    map.set(instance.id, {
      key: instance.id,
      name: instance.name,
      slot: colorSlot(instance.id),
      enabled: instance.enabled,
      messages: [],
    });
  }
  for (const message of visible) {
    const owner = owners.get(message.platform);
    const key = owner?.id ?? '\u0000unclaimed';
    let lane = map.get(key);
    if (!lane) {
      lane = {
        key,
        name: t('home.unclaimed_lane'),
        slot: null,
        enabled: true,
        messages: [],
      };
      map.set(key, lane);
    }
    lane.messages.push(message);
  }
  return [...map.values()].filter(
    (lane) => lane.enabled || lane.messages.length > 0,
  );
});

let trackWidth = $state(0);

function x(at: number): number {
  return Math.min(100, Math.max(0, ((at - start) / windowMs) * 100));
}

function seconds(ms: number): string {
  const value = ms / 1000;
  const text = value < 10 ? value.toFixed(1) : Math.round(value).toString();
  return i18n.locale === 'zh' ? `${text} 秒` : `${text}s`;
}

function label(message: TimelineMessage): string | null {
  switch (message.outcome) {
    case 'replied':
      return message.doneAt === null
        ? null
        : seconds(message.doneAt - message.at);
    case 'command':
      return message.detail ? `/${message.detail}` : t('home.outcome_command');
    case 'blocked':
      return t('home.label_blocked');
    case 'failed':
      return t('home.label_failed');
    case 'open':
      return null;
  }
}

/** Approximate rendered width of a 12.5px label, enough to keep labels from overlapping. */
function labelWidth(text: string): number {
  let width = 0;
  for (const ch of text) width += ch.charCodeAt(0) > 0x2e80 ? 12.5 : 7;
  return width;
}

/**
 * Labels that fit: walking left to right, a label is drawn only if it starts after the previous
 * one ends, so a busy lane shows some durations instead of an unreadable pile.
 */
function labelled(lane: Lane): Set<string> {
  const shown = new Set<string>();
  let lastEnd = Number.NEGATIVE_INFINITY;
  const sorted = [...lane.messages].sort((a, b) => a.at - b.at);
  for (const message of sorted) {
    const text = label(message);
    if (!text) continue;
    const left = (x(message.at) / 100) * trackWidth;
    const width = labelWidth(text);
    if (left >= lastEnd + 10 && left + width <= trackWidth + 4) {
      shown.add(message.eventId);
      lastEnd = left + width;
    }
  }
  return shown;
}

function describe(message: TimelineMessage): { title: string; text: string } {
  const title = message.channelId
    ? t('home.tip_channel', {
        platform: message.platform,
        channel: message.channelId,
      })
    : message.platform;
  let text: string;
  switch (message.outcome) {
    case 'replied':
      text = t('home.tip_replied', {
        time:
          message.doneAt === null ? '' : seconds(message.doneAt - message.at),
      });
      if (message.tools.length > 0) {
        text += t('home.tip_tools', {
          tools: [...new Set(message.tools.map((tool) => tool.name))].join(
            ', ',
          ),
        });
      }
      break;
    case 'command':
      text = t('home.tip_command', { command: message.detail ?? '' });
      break;
    case 'blocked':
      text = t('home.tip_blocked', { host: message.detail ?? '' });
      break;
    case 'failed':
      text = t('home.tip_failed', { reason: message.detail ?? '' });
      break;
    case 'open':
      text = t('home.tip_open');
      break;
  }
  return { title, text };
}

let hovered = $state<string | null>(null);

function ago(ms: number): string {
  const minutes = ms / 60000;
  if (minutes < 1) return t('home.ago_seconds', { n: Math.round(ms / 1000) });
  if (minutes < 60)
    return t('home.ago_minutes', { n: Number(minutes.toFixed(1)) });
  return t('home.ago_hours', { n: Number((minutes / 60).toFixed(1)) });
}
</script>

<section class="card px-6 pt-5 pb-4">
  <div class="mb-2 flex flex-wrap items-center justify-between gap-3">
    <div class="flex min-w-0 flex-wrap items-baseline gap-x-4 gap-y-1">
      <h2 class="m-0 text-[17px] font-semibold whitespace-nowrap">{t('home.timeline_title')}</h2>
      <span class="text-[13px] text-fg2">{t('home.timeline_hint')}</span>
    </div>
    <Seg
      size="sm"
      label={t('home.timeline_span')}
      value={span}
      onchange={(next: Span) => (span = next)}
      options={[
        { value: '1m', label: t('home.span_1m') },
        { value: '5m', label: t('home.span_5m') },
        { value: '1h', label: t('home.span_1h') },
      ]}
    />
  </div>

  {#if pipelineStore.status !== 'connected' && visible.length === 0}
    <div class="flex flex-wrap items-center justify-between gap-3 py-8">
      <p class="m-0 hint">{t('home.timeline_offline')}</p>
      <Button type="button" size="sm" onclick={() => pipelineStore.reconnect()}>
        <RefreshCw size={15} strokeWidth={2} />
        {t('home.reconnect')}
      </Button>
    </div>
  {:else if lanes.length === 0}
    <p class="m-0 py-8 hint">{t('home.timeline_no_instances')}</p>
  {:else}
    <div class="grid grid-cols-[minmax(84px,132px)_minmax(0,1fr)]">
      {#each lanes as lane, index (lane.key)}
        {@const shown = labelled(lane)}
        <div
          class="flex h-[72px] min-w-0 flex-col justify-center pr-3 {index > 0
            ? 'border-t border-dashed border-line'
            : ''}"
        >
          <span class="truncate text-[14px] font-semibold">{lane.name}</span>
          <span class="text-[12.5px] text-fg2">
            {!lane.enabled
              ? t('home.lane_stopped')
              : lane.messages.length === 1
                ? t('home.lane_count_one')
                : t('home.lane_count', { n: lane.messages.length })}
          </span>
        </div>
        <div
          class="relative h-[72px] {index > 0 ? 'border-t border-dashed border-line' : ''}"
          bind:clientWidth={trackWidth}
        >
          {#if lane.messages.length === 0}
            <span class="absolute top-[26px] left-0 text-[13px] whitespace-nowrap text-fg3">
              {t('home.lane_quiet', { span: t(`home.span_${span}`) })}
            </span>
          {/if}
          {#each lane.messages as message (message.eventId)}
            {@const left = x(message.at)}
            {@const end = message.doneAt === null ? left : x(message.doneAt)}
            {@const color = lane.slot === null ? 'var(--k-fg3)' : `var(--k-i${lane.slot})`}
            {@const text = label(message)}
            {@const info = describe(message)}
            {#if text && shown.has(message.eventId)}
              <span
                class="pointer-events-none absolute top-[14px] text-[12.5px] font-semibold whitespace-nowrap {message.outcome ===
                  'blocked' || message.outcome === 'failed'
                  ? 'text-danger'
                  : 'text-fg2'}"
                style="left: {left}%"
              >
                {text}
              </span>
            {/if}
            <button
              type="button"
              aria-label="{info.title}: {info.text}"
              onmouseenter={() => (hovered = message.eventId)}
              onmouseleave={() => (hovered = null)}
              onfocus={() => (hovered = message.eventId)}
              onblur={() => (hovered = null)}
              onclick={() => router.navigate('activity', message.eventId)}
              class="absolute top-[38px] rounded-full p-0 transition-shadow hover:shadow-[0_0_0_3px_var(--k-card),0_0_0_5px_var(--k-line)] focus-visible:shadow-[0_0_0_3px_var(--k-card),0_0_0_5px_var(--k-accent)] focus-visible:outline-none"
              style={message.outcome === 'open'
                ? `left: ${left}%; width: 12px; height: 12px; margin-top: 1px; box-shadow: inset 0 0 0 2px ${color}; background: var(--k-card)`
                : message.outcome === 'blocked'
                  ? `left: ${left}%; width: 14px; height: 14px; background: var(--k-danger)`
                  : message.outcome === 'command'
                    ? `left: ${left}%; width: max(14px, ${end - left}%); height: 14px; box-shadow: inset 0 0 0 3px ${color}; background: transparent`
                    : message.outcome === 'failed'
                      ? `left: ${left}%; width: max(14px, ${end - left}%); height: 14px; box-shadow: inset 0 0 0 3px var(--k-danger); background: transparent`
                      : `left: ${left}%; width: max(10px, ${end - left}%); height: 14px; background: ${color}`}
            >
              {#each message.tools as tool, toolIndex (toolIndex)}
                {@const offset =
                  end > left ? ((x(tool.at) - left) / (end - left)) * 100 : 50}
                <i
                  class="absolute top-[5px] h-1 w-1 rounded-full bg-white"
                  style="left: calc({Math.min(100, Math.max(0, offset))}% - 2px)"
                ></i>
              {/each}
            </button>
            {#if hovered === message.eventId}
              <div
                class="pointer-events-none absolute bottom-[42px] z-10 -translate-x-1/2 rounded-xl bg-card px-3.5 py-2 text-[13px] leading-snug whitespace-nowrap shadow-[var(--k-pop),0_0_0_1px_var(--k-line)]"
                style="left: clamp(80px, {(left + end) / 2}%, calc(100% - 80px))"
              >
                <b class="block font-semibold">{info.title}</b>
                <span class="text-fg2">{info.text}</span>
              </div>
            {/if}
          {/each}
        </div>
      {/each}
      <span></span>
      <div class="flex justify-between border-t border-line pt-1.5 text-[12px] whitespace-nowrap text-fg2">
        <span>{ago(windowMs)}</span>
        <span>{ago(windowMs / 2)}</span>
        <span>{t('home.now')}</span>
      </div>
    </div>
    {#if visible.length === 0}
      <p class="m-0 mt-3 hint">{t('home.timeline_empty')}</p>
    {/if}
  {/if}
</section>
