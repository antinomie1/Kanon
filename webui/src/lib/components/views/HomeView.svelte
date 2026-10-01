<script lang="ts">
import { Boxes, BrainCircuit, Plus, TriangleAlert } from 'lucide-svelte';
import { toggleInstance } from '../../instanceActions';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { describeReplyPolicy } from '../../stores/replyPolicy.svelte';
import { router } from '../../stores/router.svelte';
import type { TimelineMessage } from '../../timeline';
import MessageTimeline from '../home/MessageTimeline.svelte';
import EmptyState from '../ui/EmptyState.svelte';
import PageHead from '../ui/PageHead.svelte';
import Switch from '../ui/Switch.svelte';

let span = $state<'1m' | '5m' | '1h'>('5m');
let messages = $state<TimelineMessage[]>([]);

const SPAN_MINUTES = { '1m': 1, '5m': 5, '1h': 60 } as const;

const instances = $derived(instancesStore.instances);
const enabled = $derived(instances.filter((instance) => instance.enabled));
const loaded = $derived(instancesStore.catalog !== null);

const title = $derived(
  !loaded
    ? t('nav.home')
    : enabled.length === 0
      ? t('home.title_none')
      : enabled.length === 1
        ? t('home.title_one')
        : t('home.title_some', { n: enabled.length }),
);

const perMinute = $derived.by(() => {
  const value = messages.length / SPAN_MINUTES[span];
  return value >= 10 || value === 0
    ? Math.round(value)
    : Number(value.toFixed(1));
});
const replyTimes = $derived(
  messages
    .filter((m) => m.outcome === 'replied' && m.doneAt !== null)
    .map((m) => (m.doneAt as number) - m.at),
);
const averageReply = $derived(
  replyTimes.length === 0
    ? null
    : replyTimes.reduce((sum, ms) => sum + ms, 0) / replyTimes.length / 1000,
);
const blocked = $derived(
  messages.filter((m) => m.outcome === 'blocked').length,
);

// One notice per platform: several instances cannot share a platform, but a stopped platform may
// be listed for a claim that is being moved, and one warning per cause is enough.
const problems = $derived.by(() => {
  const seen = new Set<string>();
  return instancesStore.adapterProblems.filter((problem) => {
    if (seen.has(problem.platform)) return false;
    seen.add(problem.platform);
    return true;
  });
});

const modelMissing = $derived(nodeStore.health?.llm_configured === false);
</script>

<PageHead {title}>
  {#snippet sub()}
    {#if messages.length > 0}
      <span><b class="mr-1 font-extrabold text-fg tabular-nums">{perMinute}</b>{t('home.stat_per_minute')}</span>
      {#if averageReply !== null}
        <span>
          <b class="mr-1 font-extrabold text-fg tabular-nums">{t('home.seconds', { n: averageReply.toFixed(1) })}</b>{t('home.stat_average')}
        </span>
      {/if}
      <span><b class="mr-1 font-extrabold text-fg tabular-nums">{blocked}</b>{t('home.stat_blocked')}</span>
    {:else if loaded}
      <span>{t('home.stat_quiet', { span: t(`home.span_${span}`) })}</span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <button type="button" class="btn btn-primary" onclick={() => router.navigate('instances', 'new')}>
      <Plus size={16} strokeWidth={2.6} />
      {t('instances.new')}
    </button>
  {/snippet}
</PageHead>

{#if modelMissing}
  <div class="flex flex-wrap items-center gap-x-3.5 gap-y-2 rounded-[18px] bg-warn-tint py-2.5 pr-2.5 pl-3">
    <span class="grid h-[34px] w-[34px] shrink-0 place-items-center rounded-full bg-card text-warn">
      <BrainCircuit size={17} strokeWidth={2.2} />
    </span>
    <p class="m-0 min-w-0 flex-1 basis-[240px] text-[14px] text-warn-fg">
      <b class="mr-2 text-[15px] font-extrabold text-fg">{t('home.alert_model_title')}</b>
      {t('home.alert_model_text')}
    </p>
    <button type="button" class="btn btn-sm shadow-none! text-warn" onclick={() => router.navigate('models')}>
      {t('home.alert_model_action')}
    </button>
  </div>
{/if}

{#each problems as problem (problem.platform)}
  <div class="flex flex-wrap items-center gap-x-3.5 gap-y-2 rounded-[18px] bg-warn-tint py-2.5 pr-2.5 pl-3">
    <span class="grid h-[34px] w-[34px] shrink-0 place-items-center rounded-full bg-card text-warn">
      <TriangleAlert size={17} strokeWidth={2.2} />
    </span>
    <!-- Title and text share one paragraph so a narrow screen wraps the text under the title
         instead of squeezing it into a column beside it. -->
    <p class="m-0 min-w-0 flex-1 basis-[240px] text-[14px] text-warn-fg">
      <b class="mr-2 text-[15px] font-extrabold text-fg">
        {problem.reason === 'unknown'
          ? t('home.alert_unknown_title', { platform: problem.displayName })
          : t('home.alert_offline_title', {
              name: problem.instanceName,
              platform: problem.displayName,
            })}
      </b>
      {problem.reason === 'unknown'
        ? t('home.alert_unknown_text', { name: problem.instanceName })
        : t('home.alert_offline_text', { name: problem.instanceName })}
    </p>
    {#if problem.reason === 'unknown'}
      <button
        type="button"
        class="btn btn-sm shadow-none! text-warn"
        onclick={() => router.navigate('instances', problem.instanceId)}
      >
        {t('home.alert_edit_instance')}
      </button>
    {:else}
      <button
        type="button"
        class="btn btn-sm shadow-none! text-warn"
        onclick={() => router.navigate('platforms', problem.platform)}
      >
        {t('home.alert_check')}
      </button>
    {/if}
  </div>
{/each}

{#if loaded && instances.length === 0}
  <div class="card">
    <EmptyState icon={Boxes} title={t('home.empty_title')} text={t('home.empty_text')}>
      {#snippet action()}
        <button type="button" class="btn btn-primary" onclick={() => router.navigate('instances', 'new')}>
          <Plus size={16} strokeWidth={2.6} />
          {t('instances.new')}
        </button>
      {/snippet}
    </EmptyState>
  </div>
{:else if instances.length > 0}
  <div class="grid grid-cols-[repeat(auto-fill,minmax(min(100%,300px),1fr))] gap-4">
    {#each instances as instance (instance.id)}
      {@const model = instancesStore.effectiveModel(instance)}
      {@const reply = instancesStore.effectiveReplyPolicy(instance)}
      <article class="card flex flex-col gap-3.5 px-[22px] py-5 {instance.enabled ? '' : 'opacity-75'}">
        <div class="flex items-center gap-3">
          <div class="flex min-w-0 flex-col leading-[1.35]">
            <a
              href="#/instances/{encodeURIComponent(instance.id)}"
              class="truncate text-[17px] font-extrabold text-fg no-underline hover:text-accent-fg"
            >
              {instance.name}
            </a>
            <span class="flex items-center gap-1.5 text-[13px] whitespace-nowrap text-fg2">
              {#if instance.enabled}
                <i class="dot dot-ok"></i>{t('instances.state_on')}
              {:else}
                {t('instances.state_off')}
              {/if}
            </span>
          </div>
          <span class="ml-auto">
            <Switch
              checked={instance.enabled}
              disabled={instancesStore.saving}
              label={instance.enabled
                ? t('instances.stop_named', { name: instance.name })
                : t('instances.start_named', { name: instance.name })}
              onchange={() => void toggleInstance(instance)}
            />
          </span>
        </div>

        <div class="flex flex-wrap gap-1.5">
          {#each instance.adapter_status as status (status.platform)}
            {@const bad = instance.enabled && !(status.known && status.connected)}
            <span class="chip {bad ? 'chip-bad' : ''}" title={bad ? t('instances.platform_offline') : undefined}>
              {#if bad}<i class="dot dot-bad"></i>{/if}
              {status.display_name || status.platform}
            </span>
          {:else}
            <span class="chip chip-muted">{t('instances.no_platforms')}</span>
          {/each}
        </div>

        <dl class="m-0 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3.5 gap-y-1 text-[13.5px]">
          <dt class="whitespace-nowrap text-fg2">{t('instances.fact_model')}</dt>
          <dd class="m-0 truncate">
            {#if model.reference}
              <code class="text-[12.5px]">{model.reference}</code>
            {:else}
              <span class="font-bold text-warn">{t('instances.fact_no_model')}</span>
            {/if}
          </dd>
          <dt class="whitespace-nowrap text-fg2">{t('instances.fact_groups')}</dt>
          <dd class="m-0 truncate font-bold">{describeReplyPolicy(reply.policy)}</dd>
        </dl>
      </article>
    {/each}
  </div>
{/if}

{#if instances.length > 0}
  <MessageTimeline bind:span bind:messages />
{/if}
