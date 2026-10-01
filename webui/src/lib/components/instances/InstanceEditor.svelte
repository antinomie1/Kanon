<script lang="ts">
import { ChevronDown, Plus, Send, Trash2 } from 'lucide-svelte';
import { deleteInstance, toggleInstance } from '../../instanceActions';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { describeReplyPolicy } from '../../stores/replyPolicy.svelte';
import { router } from '../../stores/router.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { ReplyMode } from '../../types';
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

// Advanced settings stay open if this instance already changes any of them, so an override is
// never hidden behind a closed section.
$effect(() => {
  const current = instance;
  showAdvanced = current
    ? current.context_policy !== null ||
      current.command_policy !== null ||
      current.session_scope !== 'user' ||
      current.observe_group ||
      current.bash !== 'own_context' ||
      store.overrideCount(current) > 0
    : false;
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

function setReplyInherit(inherit: boolean) {
  if (inherit) {
    store.formReplyPolicyMode = 'inherit';
    return;
  }
  // Start the override from what currently applies, so switching the global rule off changes
  // nothing until the operator picks something else.
  const node = store.nodeReplyPolicy;
  store.formReplyPolicyMode = node?.mode ?? 'mention';
  store.formReplyProbability = node?.probability ?? 0.5;
  store.formReplyQuote = node?.quote_message ?? false;
  store.formReplyAck = node?.acknowledge ?? false;
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
        class="-mx-2 min-w-0 rounded-lg bg-transparent px-2 py-1 text-[22px] font-extrabold text-fg outline-none placeholder:text-fg3 hover:bg-sunk focus:bg-sunk focus:shadow-[inset_0_0_0_2px_var(--k-accent)]"
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
      <span class="inline-flex items-center gap-2.5 pr-1 text-[14px] font-bold whitespace-nowrap">
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
        <button type="button" class="btn btn-sm" onclick={testChat}>
          <Send size={15} strokeWidth={2.2} />
          {t('instances.test_chat')}
        </button>
        <button
          type="button"
          class="btn btn-sm btn-icon btn-danger"
          title={t('instances.delete_confirm')}
          aria-label={t('instances.delete_confirm')}
          disabled={store.saving}
          onclick={remove}
        >
          <Trash2 size={16} strokeWidth={2.2} />
        </button>
      {/if}
    </div>
  </div>

  {#if store.error && !store.saving}
    <div class="notice notice-bad mb-2" role="alert">{store.error}</div>
  {/if}

  <div class="border-t border-line">
    <Section title={t('instances.sec_platforms')} hint={t('instances.sec_platforms_hint')}>
      {#each store.formAdapters as platform (platform)}
        {@const state = platformState(platform)}
        <div
          class="flex min-h-12 flex-wrap items-center gap-x-3 gap-y-1 rounded-xl py-1.5 pr-2 pl-3.5 {state ===
          'ok'
            ? 'bg-sunk'
            : 'bg-danger-tint'}"
        >
          <i class="dot {state === 'ok' ? 'dot-ok' : 'dot-bad'}"></i>
          <span class="text-[14.5px] font-extrabold whitespace-nowrap">{platformName(platform)}</span>
          <span class="min-w-0 flex-1 truncate text-[13px] {state === 'ok' ? 'text-fg2' : 'text-danger'}">
            {state === 'ok'
              ? t('instances.platform_connected')
              : state === 'offline'
                ? t('instances.platform_offline_long')
                : t('instances.platform_unknown')}
          </span>
          {#if state === 'offline'}
            <button
              type="button"
              class="btn btn-xs"
              onclick={() => router.navigate('platforms', platform)}
            >
              {t('home.alert_check')}
            </button>
          {/if}
          <button type="button" class="btn btn-xs btn-quiet" onclick={() => store.toggleAdapter(platform)}>
            {t('instances.platform_remove')}
          </button>
        </div>
      {/each}

      {#if available.length > 0}
        <div class="flex flex-wrap gap-2">
          {#each available as adapter (adapter.platform)}
            {@const owner = store.ownerOf(adapter.platform)}
            <button
              type="button"
              class="btn btn-sm"
              disabled={Boolean(owner)}
              title={owner ? t('instances.adapter_taken', { name: owner }) : undefined}
              onclick={() => store.toggleAdapter(adapter.platform)}
            >
              <Plus size={14} strokeWidth={2.6} />
              {adapter.display_name || adapter.platform}
              {#if owner}<span class="font-semibold text-fg3">{t('instances.platform_owner', { name: owner })}</span>{/if}
            </button>
          {/each}
        </div>
      {:else if store.adapters.length === 0}
        <p class="m-0 hint">
          {t('instances.no_adapters_hint')}
          <a href="#/platforms" class="font-bold text-accent-fg">{t('nav.platforms')}</a>
        </p>
      {/if}

      {#if store.formEnabled && store.formAdapters.length === 0}
        <div class="notice notice-warn">{t('instances.warn_no_adapter')}</div>
      {/if}
    </Section>

    <Section title={t('instances.sec_brain')} hint={t('instances.sec_brain_hint')}>
      <div class="grid gap-3.5 sm:grid-cols-2">
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
          <a href="#/models" class="font-bold text-accent-fg">{t('nav.models')}</a>
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

    <Section title={t('instances.sec_groups')} hint={t('instances.sec_groups_hint')}>
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[14.5px]">
        <span class="flex-1 font-semibold">{t('instances.use_global_rule')}</span>
        {#if store.nodeReplyPolicy}
          <span class="text-[13px] text-fg2">
            {t('instances.global_is', { policy: describeReplyPolicy(store.nodeReplyPolicy) })}
          </span>
        {/if}
        <Switch
          checked={replyInherit}
          label={t('instances.use_global_rule')}
          onchange={setReplyInherit}
        />
      </div>

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
        {#if store.formReplyPolicyMode === 'probability'}
          <label class="flex items-center gap-3 text-[14.5px]">
            <span class="whitespace-nowrap">{t('instances.reply_about')}</span>
            <input
              type="range"
              min="0"
              max="1"
              step="0.05"
              bind:value={store.formReplyProbability}
              class="max-w-[360px] flex-1"
            />
            <span class="w-11 font-extrabold tabular-nums">{Math.round(store.formReplyProbability * 100)}%</span>
          </label>
        {/if}
        <div class="flex items-center gap-3 text-[14.5px]">
          <span class="flex-1">{t('reply.quote')}</span>
          <Switch
            checked={store.formReplyQuote}
            label={t('reply.quote')}
            onchange={(next) => (store.formReplyQuote = next)}
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
      {/if}
    </Section>
  </div>

  <div class="flex flex-wrap items-center gap-x-3.5 gap-y-1 border-t border-line pt-5 pb-1">
    <button
      type="button"
      class="btn btn-quiet h-8! px-0! text-accent-fg!"
      aria-expanded={showAdvanced}
      onclick={() => (showAdvanced = !showAdvanced)}
    >
      {showAdvanced ? t('instances.hide_advanced') : t('instances.show_advanced')}
      <ChevronDown size={16} strokeWidth={2.4} class="transition-transform {showAdvanced ? 'rotate-180' : ''}" />
    </button>
    {#if !showAdvanced}
      <span class="text-[13.5px] text-fg2">{t('instances.advanced_summary')}</span>
    {/if}
  </div>

  {#if showAdvanced}
    <InstanceAdvanced />
  {/if}

  {#if creating || store.changeCount > 0}
    <div class="sticky bottom-4 z-10 mt-6 flex justify-center">
      <div
        class="flex max-w-full items-center gap-3 rounded-[26px] bg-bar py-2 pr-2 pl-5 text-[14.5px] font-bold text-on-bar shadow-[var(--k-pop)]"
      >
        <span class="truncate">
          {creating
            ? t('instances.bar_new')
            : store.changeCount === 1
              ? t('instances.bar_changed_one')
              : t('instances.bar_changed', { n: store.changeCount })}
        </span>
        <button
          type="button"
          class="btn btn-quiet btn-sm text-on-bar! opacity-75 hover:opacity-100 hover:bg-transparent!"
          disabled={store.saving}
          onclick={discard}
        >
          {creating ? t('common.cancel') : t('instances.discard')}
        </button>
        <button
          type="button"
          class="btn btn-primary btn-sm"
          disabled={store.saving || !store.formName.trim()}
          onclick={save}
        >
          {store.saving
            ? t('instances.saving')
            : creating
              ? t('instances.create')
              : t('instances.save_changes')}
        </button>
      </div>
    </div>
  {/if}
</section>
