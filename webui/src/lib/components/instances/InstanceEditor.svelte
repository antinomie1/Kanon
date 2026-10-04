<script lang="ts">
import { ChevronDown, Plus, Send, Trash2 } from 'lucide-svelte';
import { deleteInstance, toggleInstance } from '../../instanceActions';
import { agentName } from '../../stores/agents.svelte';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { describeReplyPolicy } from '../../stores/replyPolicy.svelte';
import { router } from '../../stores/router.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { ReplyMode } from '../../types';
import Button from '../ui/Button.svelte';
import Section from '../ui/Section.svelte';
import Seg from '../ui/Seg.svelte';
import Select from '../ui/Select.svelte';
import Switch from '../ui/Switch.svelte';
import InstanceAdvanced from './InstanceAdvanced.svelte';

let { onSaved, onDeleted } = $props<{
  onSaved: (id: string) => void;
  onDeleted: () => void;
}>();

const store = instancesStore;
const creating = $derived(store.editingId === null);
const instance = $derived(store.find(store.editingId));

let showAdvanced = $state(false);

// Reset disclosures when selecting another instance, without reacting to unsaved field edits.
$effect(() => {
  store.editingId;
  showAdvanced = false;
});

/** Status of each claimed platform, from the node's view of the saved instance or the adapter list. */
function platformState(platform: string): 'ok' | 'offline' | 'unknown' {
  const adapter = store.adapters.find((a) => a.platform === platform);
  if (!adapter) return 'unknown';
  return adapter.connected ? 'ok' : 'offline';
}

function platformName(platform: string): string {
  return (
    store.adapters.find((a) => a.platform === platform)?.display_name ||
    platform
  );
}

const available = $derived(
  store.adapters.filter(
    (adapter) => !store.formAdapters.includes(adapter.platform),
  ),
);

const replyInherit = $derived(store.formReplyPolicyMode === 'inherit');
const simulationProbability = $derived(
  store.formConversationMode === 'simulation' &&
    (replyInherit || store.formReplyPolicyMode === 'probability'),
);
const replyProbability = $derived(
  replyInherit && simulationProbability ? 0.15 : store.formReplyProbability,
);
const replySummary = $derived(
  simulationProbability
    ? t('instances.simulation_attention_summary', { percent: Math.round(replyProbability * 100) })
    : describeReplyPolicy(replyInherit ? store.nodeReplyPolicy : {
        mode: store.formReplyPolicyMode as ReplyMode,
        probability: store.formReplyProbability,
      }),
);

/** Editing the preset starts an explicit instance override through the existing reply policy. */
function setReplyProbability(probability: number) {
  if (replyInherit) setReplyInherit(false);
  store.formReplyProbability = probability;
}


function setReplyInherit(inherit: boolean) {
  if (inherit) {
    store.formReplyPolicyMode = 'inherit';
    return;
  }
  // Start the override from what currently applies, so switching the global rule off changes
  // nothing until the operator picks something else.
  const node = store.formConversationMode === 'simulation'
    ? { mode: 'probability' as const, probability: 0.15, quote_message: false, acknowledge: false, split_lines: false, send_reasoning: false }
    : store.nodeReplyPolicy;
  store.formReplyPolicyMode = node?.mode ?? 'mention';
  store.formReplyProbability = node?.probability ?? 0.5;
  store.formReplyQuote = node?.quote_message ?? false;
  store.formReplyAck = node?.acknowledge ?? false;
  store.formReplySplitLines = node?.split_lines ?? false;
  store.formReplyReasoning = node?.send_reasoning ?? false;
}

async function save() {
  const wasCreating = creating;
  const id = await store.save();
  if (id) {
    toasts.ok(
      wasCreating
        ? t('instances.created_toast', { name: store.formName.trim() })
        : t('instances.saved_toast'),
    );
    onSaved(id);
  } else {
    toasts.error(store.error ?? t('common.error'));
  }
}

async function remove() {
  if (!instance) return;
  if (await deleteInstance(instance)) onDeleted();
}

function discard() {
  if (creating) {
    store.closeForm();
    router.navigate('instances');
  } else {
    store.discardChanges();
  }
}

function testChat() {
  if (instance) router.navigate('chat', instance.id);
}
</script>

<section class="card relative px-5 pb-6 sm:px-7">
  <div class="flex flex-wrap items-center gap-x-4 gap-y-3 pt-5 pb-4">
    <div class="flex min-w-0 flex-1 flex-col leading-tight">
      <input
        bind:value={store.formName}
        aria-label={t('instances.field_name')}
        placeholder={t('instances.name_placeholder')}
        class="-mx-2 min-w-0 rounded-lg bg-transparent px-2 py-1 text-[22px] font-semibold text-fg outline-none placeholder:text-fg3 hover:bg-sunk focus:bg-sunk focus:shadow-[inset_0_0_0_2px_var(--k-accent)]"
      />
      <span class="flex items-center gap-1.5 text-[13.5px] whitespace-nowrap text-fg2">
        {#if creating}
          {t('instances.draft_hint')}
        {:else if instance?.enabled}
          <i class="dot dot-ok"></i>{t('instances.state_on')}
        {:else}
          {t('instances.state_off')}
        {/if}
      </span>
    </div>
    <div class="flex flex-wrap items-center gap-2.5">
      <span class="inline-flex items-center gap-2.5 pr-1 text-[14px] font-medium whitespace-nowrap">
        {#if creating}
          <Switch
            checked={store.formEnabled}
            label={t('instances.power')}
            onchange={(next) => (store.formEnabled = next)}
          />
        {:else if instance}
          <Switch
            checked={instance.enabled}
            disabled={store.saving}
            label={t('instances.power')}
            onchange={() => void toggleInstance(instance)}
          />
        {/if}
        {t('instances.power')}
      </span>
      {#if instance}
        <Button type="button" size="sm" onclick={testChat}>
          <Send size={15} strokeWidth={2} />
          {t('instances.test_chat')}
        </Button>
        <Button
          type="button"
          variant="danger" size="sm" square
          title={t('instances.delete_confirm')}
          aria-label={t('instances.delete_confirm')}
          disabled={store.saving}
          onclick={remove}
        >
          <Trash2 size={16} strokeWidth={2} />
        </Button>
      {/if}
    </div>
  </div>

  {#if store.error && !store.saving}
    <div class="notice notice-bad mb-2" role="alert">{store.error}</div>
  {/if}

  {#key store.editingId}
  <div class="divide-y divide-line border-t border-line">
    <Section title={t('instances.conversation_mode')} hint={t('instances.simulation_hint')} collapsible summary={t('instances.conversation_summary', { simulation: t(store.formConversationMode === 'simulation' ? 'common.on' : 'common.off'), rules: t(store.formConversationRules ? 'common.on' : 'common.off') })}>
      <div class="flex items-center gap-3">
        <span class="flex-1">{t('instances.mode_simulation')}</span>
        <Switch checked={store.formConversationMode === 'simulation'}
          label={t('instances.mode_simulation')}
          onchange={(next) => store.setSimulationEnabled(next)} />
      </div>
      <div class="flex items-center gap-3">
        <span class="flex-1">{t('instances.simulation_behavior')}</span>
        <Switch checked={store.formConversationRules}
          label={t('instances.simulation_behavior')}
          onchange={(next) => (store.formConversationRules = next)} />
      </div>
      <p class="m-0 hint">{t('instances.conversation_rules_hint')}</p>
      {#if store.formConversationMode === 'simulation'}
        <details class="space-y-3">
          <summary class="cursor-pointer text-sm">{t('instances.simulation_details')}</summary>
        <p class="m-0 hint">{t('instances.simulation_preset')}</p>
        <p class="m-0 hint">{t('instances.simulation_scope')}</p>
        {#if store.formAdapters.some((platform) => !store.adapters.find((a) => a.platform === platform)?.capabilities.includes('group_messages'))}
          <p class="m-0 hint">{t('instances.simulation_limited_platform')}</p>
        {/if}
        {#if store.formAdapters.includes('qqofficial')}
          <p class="m-0 hint">{t('instances.simulation_qqofficial')}</p>
        {/if}
        <p class="m-0 hint">{t('instances.simulation_preview')}</p>
        </details>
        <details class="space-y-3">
          <summary class="cursor-pointer text-sm">{t('instances.simulation_tuning')}</summary>
        <div class="grid gap-3 sm:grid-cols-2">
          <label><span class="label">{t('instances.simulation_quiet')}</span>
            <input class="input" type="number" min="0" max="30000" step="100" bind:value={store.formSimulation.quiet_ms} /></label>
          <label><span class="label">{t('instances.simulation_batch')}</span>
            <input class="input" type="number" min="100" max="60000" step="100" bind:value={store.formSimulation.max_batch_ms} /></label>
          <label><span class="label">{t('instances.simulation_listen')}</span>
            <input class="input" type="number" min="1" max="120" bind:value={store.formSimulation.listen_seconds} /></label>
          <label><span class="label">{t('instances.simulation_duration')}</span>
            <input class="input" type="number" min="10" max="600" bind:value={store.formSimulation.max_participation_seconds} /></label>
          <label><span class="label">{t('instances.simulation_messages')}</span>
            <input class="input" type="number" min="1" max="20" bind:value={store.formSimulation.max_messages} /></label>
        </div>
          <Button variant="text" size="sm" onclick={() => store.resetSimulationTiming()}>{t('instances.simulation_reset')}</Button>
        </details>
      {/if}
    </Section>

    <Section title={t(store.formConversationMode === 'simulation' ? 'instances.simulation_wake' : 'instances.sec_groups')} hint={t(store.formConversationMode === 'simulation' ? 'instances.simulation_wake_hint' : 'instances.sec_groups_hint')} collapsible summary={replySummary}>
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[14.5px]">
        <span class="flex-1 font-medium">{t(store.formConversationMode === 'simulation' ? 'instances.use_simulation_rule' : 'instances.use_global_rule')}</span>
        {#if store.formConversationMode !== 'simulation' && store.nodeReplyPolicy}
          <span class="text-[13px] text-fg2">
            {t('instances.global_is', { policy: describeReplyPolicy(store.nodeReplyPolicy) })}
          </span>
        {/if}
        <Switch
          checked={replyInherit}
          label={t(store.formConversationMode === 'simulation' ? 'instances.use_simulation_rule' : 'instances.use_global_rule')}
          onchange={setReplyInherit}
        />
      </div>

      {#if simulationProbability || store.formReplyPolicyMode === 'probability'}
        <label class="flex items-center gap-3 text-[14.5px]">
          <span class="whitespace-nowrap">{t(simulationProbability ? 'instances.simulation_probability' : 'instances.reply_about')}</span>
          <input
            type="range"
            min="0"
            max="1"
            step="0.01"
            value={replyProbability}
            aria-valuetext={`${Math.round(replyProbability * 100)}%`}
            oninput={(event) => setReplyProbability(event.currentTarget.valueAsNumber)}
            class="min-w-0 max-w-[360px] flex-1"
          />
          <span class="w-11 font-semibold tabular-nums">{Math.round(replyProbability * 100)}%</span>
        </label>
        {#if simulationProbability}
          <p class="m-0 hint">{t('instances.simulation_attention')}</p>
        {/if}
      {/if}

      {#if !replyInherit}
        <div>
          <Seg
            label={t('instances.reply_mode')}
            value={store.formReplyPolicyMode as ReplyMode}
            onchange={(next: ReplyMode) => (store.formReplyPolicyMode = next)}
            options={[
              { value: 'always', label: t('instances.reply_always') },
              { value: 'mention', label: t('instances.reply_mention') },
              { value: 'probability', label: t('instances.reply_random') },
              { value: 'never', label: t('instances.reply_never') },
            ]}
          />
        </div>
        {#if store.formConversationMode === 'assistant'}
          <div class="flex items-center gap-3 text-[14.5px]">
            <span class="flex-1">{t('reply.quote')}</span>
            <Switch
              checked={store.formReplyQuote}
              label={t('reply.quote')}
              onchange={(next) => (store.formReplyQuote = next)}
            />
          </div>
          <div class="flex items-start gap-3 text-[14.5px]">
            <span class="min-w-0 flex-1">
              <span class="block">{t('reply.split_lines')}</span>
              <span class="block hint">{t('reply.split_lines_hint')}</span>
            </span>
            <Switch
              checked={store.formReplySplitLines}
              label={t('reply.split_lines')}
              onchange={(next) => (store.formReplySplitLines = next)}
            />
          </div>
          <div class="flex items-center gap-3 text-[14.5px]">
            <span class="flex-1">{t('reply.acknowledge')}</span>
            <Switch
              checked={store.formReplyAck}
              label={t('reply.acknowledge')}
              onchange={(next) => (store.formReplyAck = next)}
            />
          </div>
          <div class="flex items-center gap-3 text-[14.5px]">
            <span class="flex-1">{t('reply.reasoning')}</span>
            <Switch
              checked={store.formReplyReasoning}
              label={t('reply.reasoning')}
              onchange={(next) => (store.formReplyReasoning = next)}
            />
          </div>
        {/if}
      {/if}
    </Section>
    <Section title={t('instances.sec_platforms')} hint={t('instances.sec_platforms_hint')} collapsible summary={store.formAdapters.map(platformName).join(' · ')}>
      {#each store.formAdapters as platform (platform)}
        {@const state = platformState(platform)}
        <div
          class="flex min-h-12 flex-wrap items-center gap-x-3 gap-y-1 rounded-xl py-1.5 pr-2 pl-3.5 {state ===
          'ok'
            ? 'bg-sunk'
            : 'bg-danger-tint text-danger-fg'}"
        >
          <!-- On the error container everything, buttons included, takes on-error-container: the
               accent and the muted greys fall well under 3:1 against its vivid fill. -->
          <i class="dot {state === 'ok' ? 'dot-ok' : 'dot-bad'}"></i>
          <span class="text-[14.5px] font-semibold whitespace-nowrap">{platformName(platform)}</span>
          <span class="min-w-0 flex-1 truncate text-[13px] {state === 'ok' ? 'text-fg2' : ''}">
            {state === 'ok'
              ? t('instances.platform_connected')
              : state === 'offline'
                ? t('instances.platform_offline_long')
                : t('instances.platform_unknown')}
          </span>
          {#if state === 'offline'}
            <Button
              type="button"
              variant="outlined"
              size="xs"
              class="kanon-danger"
              onclick={() => router.navigate('platforms', platform)}
            >
              {t('home.alert_check')}
            </Button>
          {/if}
          <Button
            type="button"
            variant={state === 'ok' ? 'text' : 'danger'}
            size="xs"
            onclick={() => store.toggleAdapter(platform)}
          >
            {t('instances.platform_remove')}
          </Button>
        </div>
      {/each}

      {#if available.length > 0}
        <div class="flex flex-wrap gap-2">
          {#each available as adapter (adapter.platform)}
            {@const owner = store.ownerOf(adapter.platform)}
            <Button
              type="button"
              size="sm"
              disabled={Boolean(owner)}
              title={owner ? t('instances.adapter_taken', { name: owner }) : undefined}
              onclick={() => store.toggleAdapter(adapter.platform)}
            >
              <Plus size={14} strokeWidth={2.2} />
              {adapter.display_name || adapter.platform}
              {#if owner}<span class="font-medium text-fg3">{t('instances.platform_owner', { name: owner })}</span>{/if}
            </Button>
          {/each}
        </div>
      {:else if store.adapters.length === 0}
        <p class="m-0 hint">
          {t('instances.no_adapters_hint')}
          <a href="#/platforms" class="font-medium text-accent">{t('nav.platforms')}</a>
        </p>
      {/if}

      {#if store.formEnabled && store.formAdapters.length === 0}
        <div class="notice notice-warn">{t('instances.warn_no_adapter')}</div>
      {/if}
    </Section>

    <Section title={t('instances.sec_brain')} hint={t('instances.sec_brain_hint')} collapsible summary={`${store.formModel || store.nodeDefaultModel || t('instances.model_inherit')} · ${store.formSystemPrompt.trim() ? t('instances.field_prompt') : store.personas.find((persona) => persona.id === store.formPersonaId)?.name || t('instances.persona_none')}`}>
      <div class="grid gap-3.5 sm:grid-cols-2">
        <label class="min-w-0">
          <span class="label">{t('instances.field_agent')}</span>
          <!-- Only the built-in agent exists today, so the picker shows what answers rather than
               offering a real choice; agents added by the node later appear without a change. -->
          <Select bind:value={store.formAgent} disabled={store.agentChoices.length < 2}>
            <option value="">
              {store.nodeDefaultAgent
                ? t('instances.agent_inherit_named', {
                    agent: agentName(store.nodeDefaultAgent),
                  })
                : t('instances.agent_inherit')}
            </option>
            {#each store.agentChoices as agent (agent)}
              <option value={agent}>{agentName(agent)}</option>
            {/each}
          </Select>
        </label>
        <label class="min-w-0">
          <span class="label">{t('instances.field_model')}</span>
          <Select bind:value={store.formModel}>
            <option value="">
              {store.nodeDefaultModel
                ? t('instances.model_inherit_named', { model: store.nodeDefaultModel })
                : t('instances.model_inherit')}
            </option>
            {#each store.modelReferences as reference (reference)}
              <option value={reference}>{reference}</option>
            {/each}
          </Select>
        </label>
        <label class="min-w-0">
          <span class="label">{t('instances.field_persona')}</span>
          <Select bind:value={store.formPersonaId} disabled={store.formSystemPrompt.trim() !== ''}>
            <option value="">{t('instances.persona_none')}</option>
            {#each store.personas as persona (persona.id)}
              <option value={persona.id}>{persona.name}</option>
            {/each}
          </Select>
        </label>
      </div>
      {#if store.modelReferences.length === 0}
        <p class="m-0 hint text-warn!">
          {t('instances.model_catalog_empty')}
          <a href="#/models" class="font-medium text-accent">{t('nav.models')}</a>
        </p>
      {/if}
      <label class="block">
        <span class="label">{t('instances.field_prompt')}</span>
        <textarea
          bind:value={store.formSystemPrompt}
          rows="3"
          placeholder={t('instances.prompt_placeholder')}
          class="input"
        ></textarea>
        <span class="mt-1.5 block hint">{t('instances.prompt_hint')}</span>
      </label>
    </Section>
  </div>

  <div class="flex flex-wrap items-center gap-x-3.5 gap-y-1 border-t border-line pt-5 pb-1">
    <Button
      type="button"
      variant="text"
      size="sm"
      class="kanon-btn-flush"
      aria-expanded={showAdvanced}
      onclick={() => (showAdvanced = !showAdvanced)}
    >
      {showAdvanced ? t('instances.hide_advanced') : t('instances.show_advanced')}
      <ChevronDown size={16} strokeWidth={2} class="transition-transform {showAdvanced ? 'rotate-180' : ''}" />
    </Button>
    {#if !showAdvanced}
      <span class="text-[13.5px] text-fg2">{t('instances.advanced_summary')}</span>
    {/if}
  </div>

  {#if showAdvanced}
    <InstanceAdvanced />
  {/if}
  {/key}

  {#if creating || store.changeCount > 0}
    <div class="sticky bottom-4 z-10 mt-6 flex justify-center">
      <div
        class="flex max-w-full items-center gap-3 rounded-full bg-bar py-2 pr-2 pl-5 text-[14.5px] font-medium text-on-bar shadow-[var(--k-pop)]"
      >
        <span class="truncate">
          {creating
            ? t('instances.bar_new')
            : store.changeCount === 1
              ? t('instances.bar_changed_one')
              : t('instances.bar_changed', { n: store.changeCount })}
        </span>
        <Button
          type="button"
          variant="inverse-plain"
          size="sm"
          class="opacity-75 hover:opacity-100"
          disabled={store.saving}
          onclick={discard}
        >
          {creating ? t('common.cancel') : t('instances.discard')}
        </Button>
        <Button
          type="button"
          variant="inverse-filled"
          size="sm"
          disabled={store.saving || !store.formName.trim()}
          onclick={save}
        >
          {store.saving
            ? t('instances.saving')
            : creating
              ? t('instances.create')
              : t('instances.save_changes')}
        </Button>
      </div>
    </div>
  {/if}
</section>
