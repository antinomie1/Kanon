<script lang="ts">
import { Pause, Play, RefreshCw, Search, Trash2 } from 'lucide-svelte';
import { untrack } from 'svelte';
import { formatClock } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { logStore } from '../../stores/logs.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { pipelineStore } from '../../stores/pipeline.svelte';
import { router } from '../../stores/router.svelte';
import type { LogLevel, LogRecord, TraceRecord } from '../../types';
import { describe, STAGE_GROUPS, type StageGroup } from '../activity/describe';
import Button from '../ui/Button.svelte';
import PageHead from '../ui/PageHead.svelte';
import Seg from '../ui/Seg.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Live activity of the node: what happened to each message, and the server's own log.
 *
 * Both lists show the newest entry first, so what just happened is always in view without any
 * scrolling. Pausing freezes the list in place for reading while new entries keep arriving
 * underneath; the button says how many are waiting.
 *
 * `#/activity/<event-id>` opens the message flow searched for that one message, which is where the
 * home page's "details" link lands.
 */

type Tab = 'events' | 'logs';
let tab = $state<Tab>('events');

/**
 * Snapshot of a store's records while paused, with the store's `received` count at that moment;
 * `null` while following live.
 */
interface Frozen<T> {
  records: T[];
  at: number;
}
let frozenEvents = $state.raw<Frozen<TraceRecord> | null>(null);
let frozenLogs = $state.raw<Frozen<LogRecord> | null>(null);
const paused = $derived(
  tab === 'events' ? frozenEvents !== null : frozenLogs !== null,
);

$effect(() => {
  const param = router.param;
  if (!param) return;
  untrack(() => {
    tab = 'events';
    pipelineStore.selectedStage = 'ALL';
    pipelineStore.searchQuery = param;
  });
});

// A stream that dropped while the console was elsewhere is picked up again on arrival.
$effect(() => {
  untrack(() => {
    if (pipelineStore.status === 'disconnected') pipelineStore.reconnect();
    if (logStore.status === 'disconnected') logStore.reconnect();
  });
});

// The stores keep records oldest first; the lists show them newest first. Records are replaced
// wholesale on every arrival, so holding the old array is a free snapshot.
const events = $derived(
  (frozenEvents?.records ?? pipelineStore.records)
    .filter((rec) => pipelineStore.matches(rec))
    .reverse(),
);
const logs = $derived(
  (frozenLogs?.records ?? logStore.records)
    .filter((rec) => logStore.matches(rec))
    .reverse(),
);
/** Entries that arrived while paused, shown on the resume button. */
const waiting = $derived(
  tab === 'events'
    ? frozenEvents
      ? pipelineStore.received - frozenEvents.at
      : 0
    : frozenLogs
      ? logStore.received - frozenLogs.at
      : 0,
);

const status = $derived(
  tab === 'events' ? pipelineStore.status : logStore.status,
);
const live = $derived(
  pipelineStore.status === 'connected' && logStore.status === 'connected',
);

function togglePause() {
  if (tab === 'events') {
    frozenEvents = frozenEvents
      ? null
      : { records: pipelineStore.records, at: pipelineStore.received };
  } else {
    frozenLogs = frozenLogs
      ? null
      : { records: logStore.records, at: logStore.received };
  }
}

function clearList() {
  if (tab === 'events') {
    pipelineStore.clear();
    frozenEvents = null;
  } else {
    logStore.clear();
    frozenLogs = null;
  }
}

function reconnect() {
  if (pipelineStore.status !== 'connected') pipelineStore.reconnect();
  if (logStore.status !== 'connected') logStore.reconnect();
}

/** Narrows the message flow to one inbound message. */
function followEvent(id: string) {
  pipelineStore.selectedStage = 'ALL';
  pipelineStore.searchQuery = id;
}

const stageOptions = $derived([
  {
    value: 'ALL',
    label: t('activity.g_all', { n: pipelineStore.records.length }),
  },
  ...STAGE_GROUPS.filter(
    (group) => group !== 'breaker' || pipelineStore.stats.breaker > 0,
  ).map((group: StageGroup) => ({
    value: group,
    label: t(`activity.g_${group}`, { n: pipelineStore.stats[group] ?? 0 }),
  })),
]);

const LEVELS: (LogLevel | 'ALL')[] = ['ALL', 'ERROR', 'WARN', 'INFO', 'DEBUG'];

const LEVEL_CLASS: Record<LogLevel, string> = {
  ERROR: 'text-danger',
  WARN: 'text-warn',
  INFO: 'text-fg2',
  DEBUG: 'text-fg3',
};

/** Last two path segments of a tracing target, which name the module without the crate noise. */
function shortTarget(target: string): string {
  return target.split('::').slice(-2).join('::');
}

function fullTime(ms: number): string {
  return new Date(ms).toISOString();
}
</script>

<PageHead title={t('nav.activity')}>
  {#snippet sub()}
    <span class="flex items-center gap-2">
      {#if live}
        <i class="dot dot-ok"></i>{t('activity.live')}
      {:else}
        <i class="dot dot-warn"></i>{t('activity.not_live')}
      {/if}
    </span>
  {/snippet}
  {#snippet actions()}
    {#if !live}
      <Button type="button" size="sm" onclick={reconnect}>
        <RefreshCw size={15} strokeWidth={2} />
        {t('activity.reconnect')}
      </Button>
    {/if}
    <Seg
      label={t('nav.activity')}
      value={tab}
      onchange={(next: Tab) => (tab = next)}
      options={[
        { value: 'events', label: t('activity.tab_events') },
        { value: 'logs', label: t('activity.tab_logs') },
      ]}
    />
  {/snippet}
</PageHead>

<div class="flex flex-wrap items-center gap-2.5">
  <div class="scroll-thin max-w-full overflow-x-auto">
    {#if tab === 'events'}
      <Seg
        size="sm"
        label={t('activity.filter_stage')}
        value={pipelineStore.selectedStage}
        onchange={(next: string) => (pipelineStore.selectedStage = next)}
        options={stageOptions}
      />
    {:else}
      <Seg
        size="sm"
        label={t('activity.filter_level')}
        value={logStore.filterLevel}
        onchange={(next: LogLevel | 'ALL') => logStore.setFilter(next)}
        options={LEVELS.map((level) => ({ value: level, label: t(`activity.l_${level.toLowerCase()}`) }))}
      />
    {/if}
  </div>
  <div class="min-w-[180px] flex-1 basis-[220px] sm:max-w-[320px]">
    {#if tab === 'events'}
      <TextField
        pill
        small
        type="search"
        aria-label={t('common.search')}
        placeholder={t('activity.search_events')}
        bind:value={pipelineStore.searchQuery}
      >
        {#snippet leading()}<Search size={16} strokeWidth={2} class="text-fg3" />{/snippet}
      </TextField>
    {:else}
      <TextField
        pill
        small
        type="search"
        aria-label={t('common.search')}
        placeholder={t('activity.search_logs')}
        bind:value={logStore.searchQuery}
      >
        {#snippet leading()}<Search size={16} strokeWidth={2} class="text-fg3" />{/snippet}
      </TextField>
    {/if}
  </div>
  <div class="ml-auto flex items-center gap-2">
    <Button type="button" size="sm" aria-pressed={paused} onclick={togglePause}>
      {#if paused}
        <Play size={15} strokeWidth={2} />
        {waiting > 0 ? t('activity.resume_n', { n: waiting }) : t('activity.resume')}
      {:else}
        <Pause size={15} strokeWidth={2} />
        {t('activity.pause')}
      {/if}
    </Button>
    <Button type="button" variant="text" size="sm" onclick={clearList}>
      <Trash2 size={15} strokeWidth={2} />
      {t('activity.clear')}
    </Button>
  </div>
</div>

<section class="card scroll-thin min-h-[320px] flex-1 overflow-y-auto">
  {#if tab === 'events'}
    {#if events.length === 0}
      <div class="flex flex-col items-center gap-3 px-6 py-14 text-center">
        {#if status !== 'connected'}
          <p class="m-0 max-w-[46ch] text-[15px] font-medium">{t('activity.offline')}</p>
          <Button type="button" size="sm" onclick={reconnect}>
            <RefreshCw size={15} strokeWidth={2} />
            {t('activity.reconnect')}
          </Button>
        {:else if pipelineStore.records.length > 0}
          <p class="m-0 text-[15px] font-medium">{t('activity.no_match')}</p>
        {:else}
          <p class="m-0 max-w-[46ch] text-[15px] font-medium">{t('activity.empty_events')}</p>
          {#if (nodeStore.health?.instances?.enabled ?? 0) === 0}
            <!-- The usual reason for silence: no instance claims the platform, so inbound
                 messages are dropped before any stage is reported. -->
            <p class="m-0 max-w-[52ch] hint">{t('activity.no_instance_hint')}</p>
          {/if}
        {/if}
      </div>
    {:else}
      <ol class="m-0 list-none p-0">
        {#each events as record (record.seq)}
          {@const info = describe(record.event)}
          {@const eventId = typeof record.event.event_id === 'string' ? record.event.event_id : ''}
          <li class="grid grid-cols-[64px_minmax(0,1fr)] gap-x-3 border-t border-line px-5 py-2.5 first:border-t-0 sm:grid-cols-[76px_minmax(0,1fr)]">
            <time class="pt-px text-[12.5px] text-fg3 tabular-nums" title={fullTime(record.timestamp_ms)}>
              {formatClock(record.timestamp_ms)}
            </time>
            <div class="min-w-0">
              <div class="flex items-center gap-2">
                <i class="dot {info.tone === 'idle' ? '' : `dot-${info.tone}`}"></i>
                <span class="min-w-0 truncate text-[14.5px] font-medium {info.tone === 'bad' ? 'text-danger' : ''}">
                  {info.title}
                </span>
              </div>
              {#if info.details.length > 0 || eventId}
                <p class="m-0 mt-0.5 flex flex-wrap gap-x-3.5 pl-[15px] text-[13px] text-fg2">
                  {#each info.details as detail, i (i)}
                    <span class="min-w-0 break-words">{detail}</span>
                  {/each}
                  {#if eventId && pipelineStore.searchQuery.trim() !== eventId}
                    <button
                      type="button"
                      class="cursor-pointer font-mono text-[12px] text-fg3 hover:text-accent hover:underline"
                      title={t('activity.follow_event')}
                      onclick={() => followEvent(eventId)}
                    >
                      {eventId}
                    </button>
                  {/if}
                </p>
              {/if}
            </div>
          </li>
        {/each}
      </ol>
    {/if}
  {:else if logs.length === 0}
    <div class="flex flex-col items-center gap-3 px-6 py-14 text-center">
      {#if status !== 'connected'}
        <p class="m-0 max-w-[46ch] text-[15px] font-medium">{t('activity.offline')}</p>
        <Button type="button" size="sm" onclick={reconnect}>
          <RefreshCw size={15} strokeWidth={2} />
          {t('activity.reconnect')}
        </Button>
      {:else if logStore.records.length > 0}
        <p class="m-0 text-[15px] font-medium">{t('activity.no_match')}</p>
      {:else}
        <p class="m-0 text-[15px] font-medium">{t('activity.empty_logs')}</p>
        {#if logStore.filterLevel !== 'ALL'}
          <p class="m-0 hint">
            {t('activity.level_hint', { level: t(`activity.l_${logStore.filterLevel.toLowerCase()}`) })}
          </p>
        {/if}
      {/if}
    </div>
  {:else}
    <ol class="m-0 list-none px-0 py-1.5 font-mono text-[12.5px] leading-[1.55]">
      {#each logs as record}
        <li class="flex gap-3 px-5 py-1 hover:bg-sunk">
          <time class="shrink-0 text-fg3 tabular-nums" title={fullTime(record.timestamp_ms)}>
            {formatClock(record.timestamp_ms)}
          </time>
          <span class="w-11 shrink-0 font-medium {LEVEL_CLASS[record.level]}">{record.level}</span>
          <span class="hidden w-[180px] shrink-0 truncate text-fg3 md:inline" title={record.target}>
            {shortTarget(record.target)}
          </span>
          <span class="min-w-0 break-words text-fg">{record.message}</span>
        </li>
      {/each}
    </ol>
  {/if}
</section>
