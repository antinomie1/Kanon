<script lang="ts">
import {
  AlertCircle,
  Bot,
  CheckCircle2,
  Cpu,
  MessageSquarePlus,
  Pencil,
  Plus,
  Power,
  Trash2,
  X,
} from 'lucide-svelte';
import { describeContextPolicy } from '../../stores/contextPolicy.svelte';
import { t } from '../../stores/i18n.svelte';
import type { PolicyKind } from '../../stores/instances.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { describeReplyPolicy } from '../../stores/replyPolicy.svelte';
import type { ItemPolicy } from '../../types';

/** Policy kinds in the order the form renders them. */
const policyKinds: PolicyKind[] = ['plugins', 'skills', 'mcp'];

/** Three-way choices offered per item. */
const policyChoices: { value: ItemPolicy; labelKey: string }[] = [
  { value: 'inherit', labelKey: 'instances.policy_inherit' },
  { value: 'enable', labelKey: 'instances.policy_enable' },
  { value: 'disable', labelKey: 'instances.policy_disable' },
];

/** Reply-policy choices, in the order the form renders them. */
const replyChoices: { value: string; labelKey: string }[] = [
  { value: 'inherit', labelKey: 'reply.inherit' },
  { value: 'always', labelKey: 'reply.mode_always' },
  { value: 'mention', labelKey: 'reply.mode_mention' },
  { value: 'probability', labelKey: 'reply.mode_probability' },
  { value: 'never', labelKey: 'reply.mode_never' },
];

/** Section heading key for one policy kind. */
function policyTitleKey(kind: PolicyKind): string {
  switch (kind) {
    case 'plugins':
      return 'instances.section_plugins';
    case 'skills':
      return 'instances.section_skills';
    case 'mcp':
      return 'instances.section_mcp';
  }
}

// Load once when the view mounts; the catalog is small and changes only through this view. The
// model catalog is loaded alongside it so the model picker can offer canonical references.
$effect(() => {
  void instancesStore.load();
  void modelsStore.load();
});
</script>

<div class="p-6 space-y-6 max-w-7xl mx-auto font-sans">
  <!-- Gate banner: this is the switch that decides whether anything is answered at all. -->
  <div
    class="border rounded-xl p-4 sm:p-5 shadow-xs flex flex-wrap items-start justify-between gap-4
      {instancesStore.enabledCount > 0
      ? 'bg-white dark:bg-zinc-900 border-zinc-200 dark:border-zinc-800'
      : 'bg-amber-50 dark:bg-amber-950/30 border-amber-300 dark:border-amber-800'}"
  >
    <div class="flex items-start gap-3.5">
      <div
        class="p-2.5 rounded-xl {instancesStore.enabledCount > 0
          ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
          : 'bg-amber-500/15 text-amber-600 dark:text-amber-400'}"
      >
        <Bot class="w-6 h-6" />
      </div>
      <div>
        <div class="flex items-center gap-2.5 flex-wrap">
          <span class="text-sm text-zinc-500 font-medium">{t('instances.gate_label')}</span>
          {#if instancesStore.enabledCount > 0}
            <code
              class="px-2.5 py-1 rounded-lg font-mono text-sm font-bold bg-emerald-50 dark:bg-emerald-950/80 text-emerald-600 dark:text-emerald-400 border border-emerald-200 dark:border-emerald-800/60"
            >
              {instancesStore.enabledCount} / {instancesStore.instances.length}
            </code>
          {:else}
            <span class="text-sm font-medium text-amber-700 dark:text-amber-300">
              {t('instances.gate_none')}
            </span>
          {/if}
        </div>
        <p class="text-xs text-zinc-500 mt-1 leading-relaxed">
          {t('instances.gate_hint')}
        </p>
      </div>
    </div>

    <button
      onclick={() => instancesStore.openCreate()}
      class="px-3.5 py-2 bg-indigo-600 hover:bg-indigo-700 text-white rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer shadow-2xs"
    >
      <Plus class="w-4 h-4" />
      <span>{t('instances.new')}</span>
    </button>
  </div>

  {#if instancesStore.error}
    <p class="text-sm text-rose-600 dark:text-rose-400 flex items-center gap-2">
      <AlertCircle class="w-4 h-4" /> {instancesStore.error}
    </p>
  {/if}
  {#if instancesStore.notice}
    <p class="text-sm text-emerald-600 dark:text-emerald-400 flex items-center gap-2">
      <CheckCircle2 class="w-4 h-4" /> {instancesStore.notice}
    </p>
  {/if}

  <!-- Instance list -->
  {#if instancesStore.loading && instancesStore.instances.length === 0}
    <p class="text-sm text-zinc-400">{t('instances.loading')}</p>
  {:else if instancesStore.instances.length === 0}
    <div
      class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-2xl p-12 text-center shadow-xs space-y-5"
    >
      <div
        class="w-16 h-16 rounded-2xl bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 flex items-center justify-center mx-auto text-zinc-400"
      >
        <Bot class="w-8 h-8 stroke-[1.5]" />
      </div>
      <div class="max-w-lg mx-auto space-y-2">
        <h3 class="text-lg font-semibold text-zinc-900 dark:text-zinc-100">
          {t('instances.empty_title')}
        </h3>
        <p class="text-sm text-zinc-500 leading-relaxed">{t('instances.empty_hint')}</p>
      </div>
      <button
        onclick={() => instancesStore.openCreate()}
        class="px-5 py-2.5 bg-indigo-600 hover:bg-indigo-700 text-white rounded-xl text-sm font-medium inline-flex items-center gap-2 transition cursor-pointer shadow-xs"
      >
        <Plus class="w-4.5 h-4.5" />
        <span>{t('instances.new')}</span>
      </button>
    </div>
  {:else}
    <div class="grid grid-cols-1 gap-4">
      {#each instancesStore.instances as instance (instance.id)}
        <div
          class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-4 sm:p-5 shadow-xs space-y-4"
        >
          <div class="flex flex-wrap items-start justify-between gap-4">
            <div class="flex items-start gap-3.5">
              <div
                class="p-2.5 rounded-xl {instance.enabled
                  ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
                  : 'bg-zinc-100 dark:bg-zinc-800 text-zinc-400'}"
              >
                <Bot class="w-5 h-5" />
              </div>
              <div>
                <div class="flex items-center gap-2.5 flex-wrap">
                  <span class="font-semibold text-zinc-900 dark:text-zinc-100">{instance.name}</span>
                  <code class="text-xs font-mono text-zinc-400">{instance.id}</code>
                  <span
                    class="px-2 py-0.5 rounded-md text-xs font-medium border
                      {instance.enabled
                      ? 'border-emerald-200 dark:border-emerald-800/60 text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-950/60'
                      : 'border-zinc-200 dark:border-zinc-700 text-zinc-500'}"
                  >
                    {instance.enabled ? t('instances.running') : t('instances.stopped')}
                  </span>
                </div>

                <div class="flex flex-wrap items-center gap-2 mt-2">
                  {#each instance.adapter_status as adapter (adapter.platform)}
                    <span
                      class="px-2 py-0.5 rounded-md text-xs font-mono border flex items-center gap-1.5
                        {adapter.known && adapter.connected
                        ? 'border-emerald-200 dark:border-emerald-800/60 text-emerald-600 dark:text-emerald-400'
                        : 'border-zinc-200 dark:border-zinc-700 text-zinc-500'}"
                      title={adapter.known
                        ? `${adapter.display_name} (${adapter.kind})`
                        : t('instances.adapter_unknown')}
                    >
                      <span
                        class="w-1.5 h-1.5 rounded-full {adapter.known && adapter.connected
                          ? 'bg-emerald-500'
                          : 'bg-zinc-400'}"
                      ></span>
                      {adapter.platform}
                    </span>
                  {/each}
                  {#if instance.adapters.length === 0}
                    <span class="text-xs text-zinc-400">{t('instances.no_adapter')}</span>
                  {/if}
                </div>

                <div class="flex flex-wrap items-center gap-3 mt-2 text-xs text-zinc-500">
                  <span class="flex items-center gap-1.5">
                    <Cpu class="w-3.5 h-3.5" />
                    {instance.model ?? t('instances.model_default')}
                  </span>
                  {#if instance.reply_policy}
                    <span>
                      · {t('instances.reply_policy_badge', {
                        policy: describeReplyPolicy(instance.reply_policy),
                      })}
                    </span>
                  {:else}
                    <span>· {t('instances.reply_policy')}: {t('reply.inherit')}</span>
                  {/if}
                  {#if instance.persona_id}
                    <span>· {t('instances.persona_label')}: {instance.persona_id}</span>
                  {/if}
                  {#if instance.system_prompt}
                    <span>· {t('instances.custom_prompt')}</span>
                  {/if}
                  {#if instancesStore.overrideCount(instance) > 0}
                    <span>
                      ·
                      {t('instances.overrides').replace(
                        '{count}',
                        String(instancesStore.overrideCount(instance)),
                      )}
                    </span>
                  {/if}
                </div>
              </div>
            </div>

            <div class="flex items-center gap-2">
              <button
                onclick={() => instancesStore.toggleEnabled(instance)}
                disabled={instancesStore.saving}
                class="px-3 py-1.5 rounded-lg text-xs font-medium border transition cursor-pointer disabled:opacity-50
                  {instance.enabled
                  ? 'border-zinc-200 dark:border-zinc-700 text-zinc-600 dark:text-zinc-300 hover:border-zinc-400'
                  : 'border-emerald-300 dark:border-emerald-800 text-emerald-600 dark:text-emerald-400 hover:bg-emerald-50 dark:hover:bg-emerald-950/40'}"
              >
                <span class="flex items-center gap-1.5">
                  <Power class="w-3.5 h-3.5" />
                  {instance.enabled ? t('instances.stop') : t('instances.start')}
                </span>
              </button>
              <button
                onclick={() => instancesStore.openEdit(instance)}
                class="p-1.5 rounded-lg text-zinc-500 hover:text-indigo-600 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition cursor-pointer"
                title={t('instances.edit')}
              >
                <Pencil class="w-4 h-4" />
              </button>
              <button
                onclick={() => instancesStore.remove(instance.id)}
                disabled={instancesStore.saving}
                class="p-1.5 rounded-lg text-zinc-500 hover:text-rose-600 hover:bg-rose-50 dark:hover:bg-rose-950/30 transition cursor-pointer disabled:opacity-50"
                title={t('instances.delete')}
              >
                <Trash2 class="w-4 h-4" />
              </button>
            </div>
          </div>
        </div>
      {/each}
    </div>
  {/if}

  <!-- Create / edit modal -->
  {#if instancesStore.isFormOpen}
    <div class="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/40 backdrop-blur-sm">
      <div
        class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-2xl shadow-xl w-full max-w-2xl max-h-[90vh] overflow-y-auto"
      >
        <div
          class="flex items-center justify-between px-5 py-4 border-b border-zinc-200 dark:border-zinc-800"
        >
          <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">
            {instancesStore.editingId ? t('instances.edit_title') : t('instances.new_title')}
          </h3>
          <button
            onclick={() => instancesStore.closeForm()}
            class="p-1.5 rounded-lg text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 transition cursor-pointer"
          >
            <X class="w-4.5 h-4.5" />
          </button>
        </div>

        <div class="p-5 space-y-5">
          {#if instancesStore.error}
            <p class="text-sm text-rose-600 dark:text-rose-400 flex items-center gap-2">
              <AlertCircle class="w-4 h-4" /> {instancesStore.error}
            </p>
          {/if}

          <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <label class="space-y-1.5">
              <span class="text-xs font-medium text-zinc-500">{t('instances.field_name')}</span>
              <input
                bind:value={instancesStore.formName}
                placeholder="黑猪AI"
                class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg focus:outline-hidden focus:border-indigo-400"
              />
            </label>
            <label class="space-y-1.5">
              <span class="text-xs font-medium text-zinc-500">{t('instances.field_enabled')}</span>
              <button
                type="button"
                onclick={() => (instancesStore.formEnabled = !instancesStore.formEnabled)}
                class="w-full px-3 py-2 text-sm rounded-lg border transition cursor-pointer flex items-center justify-between
                  {instancesStore.formEnabled
                  ? 'border-emerald-300 dark:border-emerald-800 text-emerald-600 dark:text-emerald-400 bg-emerald-50 dark:bg-emerald-950/40'
                  : 'border-zinc-200 dark:border-zinc-700 text-zinc-500'}"
              >
                <span>{instancesStore.formEnabled ? t('instances.running') : t('instances.stopped')}</span>
                <Power class="w-4 h-4" />
              </button>
            </label>
          </div>

          <div class="space-y-2">
            <span class="text-xs font-medium text-zinc-500">{t('instances.field_adapters')}</span>
            <p class="text-xs text-zinc-400">{t('instances.adapters_hint')}</p>
            <div class="flex flex-wrap gap-2">
              {#each instancesStore.adapters as adapter (adapter.platform)}
                {@const owner = instancesStore.ownerOf(adapter.platform)}
                <button
                  type="button"
                  onclick={() => instancesStore.toggleAdapter(adapter.platform)}
                  disabled={Boolean(owner)}
                  title={owner ? t('instances.adapter_taken').replace('{name}', owner) : adapter.display_name}
                  class="px-3 py-1.5 rounded-lg text-xs font-mono border transition cursor-pointer disabled:opacity-40 disabled:cursor-not-allowed
                    {instancesStore.formAdapters.includes(adapter.platform)
                    ? 'border-indigo-400 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/40'
                    : 'border-zinc-200 dark:border-zinc-700 text-zinc-600 dark:text-zinc-300'}"
                >
                  <span class="flex items-center gap-1.5">
                    <span
                      class="w-1.5 h-1.5 rounded-full {adapter.connected ? 'bg-emerald-500' : 'bg-zinc-400'}"
                    ></span>
                    {adapter.platform}
                    <span class="text-[10px] text-zinc-400">({adapter.kind})</span>
                  </span>
                </button>
              {/each}
              {#if instancesStore.adapters.length === 0}
                <span class="text-xs text-zinc-400">{t('instances.no_adapters')}</span>
              {/if}
            </div>
          </div>

          <label class="space-y-1.5 block">
            <span class="text-xs font-medium text-zinc-500">{t('instances.field_persona')}</span>
            <select
              bind:value={instancesStore.formPersonaId}
              class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg focus:outline-hidden cursor-pointer"
            >
              <option value="">{t('instances.persona_none')}</option>
              {#each instancesStore.personas as persona (persona.id)}
                <option value={persona.id}>{persona.name}</option>
              {/each}
            </select>
          </label>

          <label class="space-y-1.5 block">
            <span class="text-xs font-medium text-zinc-500">{t('instances.field_prompt')}</span>
            <textarea
              bind:value={instancesStore.formSystemPrompt}
              rows="3"
              placeholder={t('instances.prompt_placeholder')}
              class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg focus:outline-hidden focus:border-indigo-400 resize-y"
            ></textarea>
            <span class="text-xs text-zinc-400">{t('instances.prompt_hint')}</span>
          </label>

          <label class="space-y-1.5 block">
            <span class="text-xs font-medium text-zinc-500">{t('instances.field_model')}</span>
            <select
              bind:value={instancesStore.formModel}
              class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg focus:outline-hidden cursor-pointer"
            >
              <option value="">
                {instancesStore.nodeDefaultModel
                  ? t('instances.model_inherit_named', {
                      model: instancesStore.nodeDefaultModel,
                    })
                  : t('instances.model_inherit')}
              </option>
              {#each instancesStore.modelReferences as reference (reference)}
                <option value={reference}>{reference}</option>
              {/each}
            </select>
            <span class="text-xs text-zinc-400">{t('instances.model_hint')}</span>
            {#if instancesStore.modelReferences.length === 0}
              <span class="text-xs text-amber-600 dark:text-amber-400">
                {t('instances.model_catalog_empty')}
              </span>
            {/if}
          </label>

          <!-- Reply policy: deciding whether group messages are answered at all. -->
          <div class="space-y-2 border-t border-zinc-200 dark:border-zinc-800 pt-4">
            <div>
              <span class="text-xs font-medium text-zinc-500">{t('instances.reply_policy')}</span>
              <p class="text-xs text-zinc-400 mt-0.5">{t('instances.reply_policy_hint')}</p>
            </div>

            <select
              bind:value={instancesStore.formReplyPolicyMode}
              class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg focus:outline-hidden cursor-pointer"
            >
              {#each replyChoices as choice (choice.value)}
                <option value={choice.value}>{t(choice.labelKey)}</option>
              {/each}
            </select>

            {#if instancesStore.formReplyPolicyMode === 'probability'}
              <div class="flex items-center gap-3">
                <input
                  type="range"
                  min="0"
                  max="1"
                  step="0.05"
                  bind:value={instancesStore.formReplyProbability}
                  class="flex-1 accent-indigo-600 cursor-pointer"
                  aria-label={t('reply.probability')}
                />
                <span class="w-12 text-right text-xs font-mono text-zinc-600 dark:text-zinc-300">
                  {Math.round(instancesStore.formReplyProbability * 100)}%
                </span>
              </div>
            {/if}

            {#if instancesStore.formReplyPolicyMode === 'inherit'}
              <p class="text-xs text-zinc-400">
                {t('instances.reply_inherit_hint')}
                {#if instancesStore.nodeReplyPolicy}
                  <span class="font-mono text-zinc-500">
                    {t('reply.node_current', {
                      policy: describeReplyPolicy(instancesStore.nodeReplyPolicy),
                    })}
                  </span>
                {/if}
              </p>
            {:else}
              <p class="text-xs text-zinc-400">{t('instances.reply_override_hint')}</p>
            {/if}
          </div>

          <!-- Context extras: whether the sender id and the message time reach the model. -->
          <div class="space-y-3 border-t border-zinc-200 dark:border-zinc-800 pt-4">
            <div>
              <span class="text-xs font-medium text-zinc-500">{t('context.title')}</span>
              <p class="text-xs text-zinc-400 mt-0.5">{t('context.hint')}</p>
            </div>

            <label class="flex items-center gap-2 text-sm text-zinc-700 dark:text-zinc-300 cursor-pointer select-none">
              <input
                type="checkbox"
                bind:checked={instancesStore.formContextInherit}
                class="rounded text-indigo-600 focus:ring-0 w-4 h-4"
              />
              <span>{t('reply.inherit')}</span>
            </label>

            <div class="space-y-2 {instancesStore.formContextInherit ? 'opacity-50 pointer-events-none' : ''}">
              <label class="flex items-center justify-between gap-4 text-sm text-zinc-700 dark:text-zinc-300 cursor-pointer select-none">
                <span>{t('context.channel_id')}</span>
                <input
                  type="checkbox"
                  bind:checked={instancesStore.formIncludeChannelId}
                  class="rounded text-indigo-600 focus:ring-0 w-4 h-4"
                />
              </label>
              <label class="flex items-center justify-between gap-4 text-sm text-zinc-700 dark:text-zinc-300 cursor-pointer select-none">
                <span>{t('context.sender_id')}</span>
                <input
                  type="checkbox"
                  bind:checked={instancesStore.formIncludeSenderId}
                  class="rounded text-indigo-600 focus:ring-0 w-4 h-4"
                />
              </label>
              <label class="flex items-center justify-between gap-4 text-sm text-zinc-700 dark:text-zinc-300 cursor-pointer select-none">
                <span>{t('context.timestamp')}</span>
                <input
                  type="checkbox"
                  bind:checked={instancesStore.formIncludeTimestamp}
                  class="rounded text-indigo-600 focus:ring-0 w-4 h-4"
                />
              </label>
            </div>

            {#if instancesStore.formContextInherit && instancesStore.nodeContextPolicy}
              <p class="text-xs text-zinc-400">
                {t('reply.node_current', {
                  policy: describeContextPolicy(instancesStore.nodeContextPolicy),
                })}
              </p>
            {/if}
          </div>

          <!-- Per-item policy: the node-wide switch still wins; these only restrict further or
               document an explicit opt-in for this instance. -->
          <div class="space-y-3 border-t border-zinc-200 dark:border-zinc-800 pt-4">
            <div>
              <span class="text-xs font-medium text-zinc-500">{t('instances.items_title')}</span>
              <p class="text-xs text-zinc-400 mt-0.5">{t('instances.items_hint')}</p>
            </div>

            {#each policyKinds as kind (kind)}
              {@const items = instancesStore.itemsOf(kind)}
              <div class="space-y-1.5">
                <span class="text-xs font-semibold text-zinc-600 dark:text-zinc-300">
                  {t(policyTitleKey(kind))}
                </span>
                {#if items.length === 0}
                  <p class="text-xs text-zinc-400">{t('instances.no_items')}</p>
                {:else}
                  <div class="space-y-1">
                    {#each items as item (item.id)}
                      <div class="flex items-center justify-between gap-3">
                        <span class="text-xs text-zinc-600 dark:text-zinc-300 truncate" title={item.id}>
                          {item.name}
                        </span>
                        <div class="flex rounded-lg bg-zinc-100 dark:bg-zinc-800/80 p-0.5 shrink-0">
                          {#each policyChoices as choice (choice.value)}
                            <button
                              type="button"
                              onclick={() => instancesStore.setPolicy(kind, item.id, choice.value)}
                              class="px-2 py-0.5 text-[11px] rounded-md transition cursor-pointer {instancesStore.policyOf(
                                kind,
                                item.id,
                              ) === choice.value
                                ? 'bg-white dark:bg-zinc-700 text-zinc-900 dark:text-zinc-100 shadow-2xs'
                                : 'text-zinc-500 hover:text-zinc-800 dark:hover:text-zinc-200'}"
                            >
                              {t(choice.labelKey)}
                            </button>
                          {/each}
                        </div>
                      </div>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
          </div>

          {#if instancesStore.formEnabled && instancesStore.formAdapters.length === 0}
            <p class="text-xs text-amber-600 dark:text-amber-400 flex items-center gap-1.5">
              <AlertCircle class="w-3.5 h-3.5" />
              {t('instances.warn_no_adapter')}
            </p>
          {/if}
        </div>

        <div
          class="flex items-center justify-end gap-2 px-5 py-4 border-t border-zinc-200 dark:border-zinc-800"
        >
          <button
            onclick={() => instancesStore.closeForm()}
            class="px-3.5 py-2 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 text-zinc-600 dark:text-zinc-300 transition cursor-pointer"
          >
            {t('instances.cancel')}
          </button>
          <button
            onclick={() => instancesStore.save()}
            disabled={instancesStore.saving || !instancesStore.formName.trim()}
            class="px-3.5 py-2 text-sm rounded-lg bg-indigo-600 hover:bg-indigo-700 text-white font-medium flex items-center gap-2 transition cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <MessageSquarePlus class="w-4 h-4" />
            {instancesStore.saving ? t('instances.saving') : t('instances.save')}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>
