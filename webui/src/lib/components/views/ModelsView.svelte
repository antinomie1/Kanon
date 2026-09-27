<script lang="ts">
import {
  AlertCircle,
  Check,
  Cpu,
  Pencil,
  Plus,
  RefreshCw,
  Star,
  Trash2,
  X,
} from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import {
  CAPABILITY_FLAGS,
  DEFAULT_CAPABILITIES,
  modelsStore,
} from '../../stores/models.svelte';
import type { ModelCapabilities, ModelSpec } from '../../types';

/**
 * Form-shaped draft of one catalog entry.
 *
 * Numeric fields are strings so an empty input means "not configured" instead of `0`, which the
 * wire contract distinguishes; they are converted on save.
 */
interface ModelDraft {
  provider: string;
  model: string;
  display_name: string;
  context_length: string;
  max_output_tokens: string;
  temperature: string;
  capabilities: ModelCapabilities;
}

function toDraft(spec: ModelSpec): ModelDraft {
  return {
    provider: spec.provider,
    model: spec.model,
    display_name: spec.display_name ?? '',
    context_length: spec.context_length?.toString() ?? '',
    max_output_tokens: spec.max_output_tokens?.toString() ?? '',
    temperature: spec.temperature?.toString() ?? '',
    capabilities: { ...spec.capabilities },
  };
}

function blankDraft(provider?: string): ModelDraft {
  return {
    provider: provider ?? modelsStore.providers[0] ?? '',
    model: '',
    display_name: '',
    context_length: '',
    max_output_tokens: '',
    temperature: '',
    capabilities: { ...DEFAULT_CAPABILITIES },
  };
}

/** Optional numeric input converted to the wire form; blank means "not configured". */
function optionalNumber(raw: string): number | undefined {
  const value = Number(raw.trim());
  return raw.trim() !== '' && Number.isFinite(value) ? value : undefined;
}

let { provider = '' }: { provider?: string } = $props();

let draft = $state<ModelDraft>(blankDraft());
/** Canonical reference of the row being edited, or `null` when no inline editor is open. */
let editingRef = $state<string | null>(null);
let isAdding = $state(false);
let filterProvider = $state('');

// Load once; the providers view keeps the catalog fresh after every endpoint edit.
$effect(() => {
  if (!modelsStore.catalog) void modelsStore.load();
});

// Embedded in a provider panel, the list is scoped to that provider.
$effect(() => {
  filterProvider = provider;
});

let visibleModels = $derived(
  filterProvider
    ? modelsStore.models.filter((spec) => spec.provider === filterProvider)
    : modelsStore.models,
);

function startAdd() {
  draft = blankDraft(provider || filterProvider || undefined);
  editingRef = null;
  isAdding = true;
}

function startEdit(spec: ModelSpec) {
  draft = toDraft(spec);
  isAdding = false;
  editingRef = modelsStore.referenceOf(spec);
}

function cancelEdit() {
  editingRef = null;
  isAdding = false;
}

function toggleCapability(flag: keyof ModelCapabilities) {
  draft.capabilities = {
    ...draft.capabilities,
    [flag]: !draft.capabilities[flag],
  };
}

async function saveDraft() {
  if (!draft.provider.trim() || !draft.model.trim()) return;

  const spec: ModelSpec = {
    provider: draft.provider.trim(),
    model: draft.model.trim(),
    capabilities: { ...draft.capabilities },
    // The server forces this value, but sending it keeps the payload self-describing.
    source: 'manual',
  };
  if (draft.display_name.trim()) spec.display_name = draft.display_name.trim();
  const contextLength = optionalNumber(draft.context_length);
  if (contextLength !== undefined) spec.context_length = contextLength;
  const maxOutput = optionalNumber(draft.max_output_tokens);
  if (maxOutput !== undefined) spec.max_output_tokens = maxOutput;
  const temperature = optionalNumber(draft.temperature);
  if (temperature !== undefined) spec.temperature = temperature;

  const ok = await modelsStore.upsert(spec);
  if (ok) cancelEdit();
}

async function removeModel(reference: string) {
  if (!confirm(t('models.delete_confirm'))) return;
  await modelsStore.remove(reference);
}

function sourceLabel(source: ModelSpec['source']): string {
  return t(`models.source_${source ?? 'unknown'}`);
}
</script>

<div class="{provider ? 'space-y-4' : 'p-6 space-y-6 max-w-7xl mx-auto'} font-sans">
  <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-4 sm:p-5 shadow-xs flex flex-wrap items-center justify-between gap-4">
    {#if !provider}
    <!-- Node default: the reference every instance without an override resolves to. -->
    <div class="flex items-center gap-3.5">
      <div class="p-2.5 rounded-xl bg-amber-500/10 text-amber-600 dark:text-amber-400">
        <Star class="w-6 h-6" />
      </div>
      <div>
        <span class="text-sm text-zinc-500 font-medium">{t('models.default_model')}:</span>
        {#if modelsStore.defaultModel}
          <code class="ml-2.5 px-2.5 py-1 rounded-lg font-mono text-sm font-bold bg-amber-50 dark:bg-amber-950/60 text-amber-700 dark:text-amber-400 border border-amber-200 dark:border-amber-800/60">
            {modelsStore.defaultModel}
          </code>
        {:else}
          <span class="ml-2.5 text-sm text-zinc-400 font-mono">{t('models.default_none')}</span>
        {/if}
        <p class="text-xs text-zinc-400 mt-1 font-mono">
          {t('models.total')}: {modelsStore.catalog?.total ?? 0}
        </p>
      </div>
    </div>
    {:else}
    <div class="min-w-0">
      <h4 class="text-sm font-semibold text-zinc-900 dark:text-zinc-100">{t('models.title')}</h4>
      <p class="text-xs text-zinc-500 font-mono mt-1">
        {provider}/&lt;model-id&gt; · {visibleModels.length}
      </p>
    </div>
    {/if}

    <div class="flex flex-wrap items-center gap-3">
      {#if !provider}
        <div class="flex items-center gap-2">
          <label for="models-provider-filter" class="text-xs text-zinc-500">{t('models.filter_provider')}:</label>
          <select
            id="models-provider-filter"
            bind:value={filterProvider}
            class="px-3 py-1.5 text-xs sm:text-sm font-mono bg-zinc-50 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden cursor-pointer"
          >
            <option value="">{t('models.all_providers')}</option>
            {#each modelsStore.providers as providerName (providerName)}
              <option value={providerName}>{providerName}</option>
            {/each}
          </select>
        </div>
      {/if}

      <button
        onclick={startAdd}
        disabled={modelsStore.providers.length === 0}
        class="px-3.5 py-2 bg-indigo-600 hover:bg-indigo-700 text-white rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer shadow-2xs disabled:opacity-50 disabled:cursor-not-allowed"
      >
        <Plus class="w-4 h-4" />
        <span>{t('models.add')}</span>
      </button>
    </div>
  </div>

  {#if modelsStore.error}
    <p class="text-sm text-rose-600 dark:text-rose-400 flex items-center gap-2">
      <AlertCircle class="w-4 h-4" /> {modelsStore.error}
    </p>
  {/if}

  {#if modelsStore.loading && modelsStore.models.length === 0}
    <p class="text-sm text-zinc-400">{t('common.loading')}</p>
  {:else if modelsStore.models.length === 0 && !isAdding}
    <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-2xl p-12 text-center shadow-xs space-y-5">
      <div class="w-16 h-16 rounded-2xl bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 flex items-center justify-center mx-auto text-zinc-400">
        <Cpu class="w-8 h-8 stroke-[1.5]" />
      </div>
      <div class="max-w-lg mx-auto space-y-2">
        <h3 class="text-lg font-semibold text-zinc-900 dark:text-zinc-100">{t('models.empty')}</h3>
        <p class="text-sm text-zinc-500 leading-relaxed">{t('models.empty_hint')}</p>
      </div>
      <button
        onclick={startAdd}
        disabled={modelsStore.providers.length === 0}
        class="px-5 py-2.5 bg-indigo-600 hover:bg-indigo-700 text-white rounded-xl text-sm font-medium inline-flex items-center gap-2 transition cursor-pointer shadow-xs disabled:opacity-50"
      >
        <Plus class="w-4.5 h-4.5" />
        <span>{t('models.add')}</span>
      </button>
    </div>
  {:else}
    <div class="space-y-2.5">
      {#each visibleModels as spec (modelsStore.referenceOf(spec))}
        {@const reference = modelsStore.referenceOf(spec)}
        {@const isEditing = editingRef === reference}
        {@const isDefault = modelsStore.defaultModel === reference}

        <div
          class="bg-white dark:bg-zinc-900 border rounded-xl p-4 shadow-xs transition
            {isEditing
              ? 'border-indigo-300 dark:border-indigo-800 ring-1 ring-indigo-200 dark:ring-indigo-900'
              : isDefault
                ? 'border-amber-200 dark:border-amber-800/60'
                : 'border-zinc-200 dark:border-zinc-800'}"
        >
          {#if isEditing}
            <!-- Inline editor -->
            <div class="space-y-4">
              <div class="flex items-center justify-between gap-3">
                <code class="font-mono font-bold text-sm text-indigo-600 dark:text-indigo-400">
                  {reference}
                </code>
                <span class="text-xs text-zinc-400 font-mono">{t('models.source_manual')}</span>
              </div>

              <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3 text-sm">
                <label class="space-y-1">
                  <span class="text-xs text-zinc-500">{t('models.display_name')}</span>
                  <input
                    type="text"
                    bind:value={draft.display_name}
                    placeholder={t('models.display_name_placeholder')}
                    class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                </label>
                <label class="space-y-1">
                  <span class="text-xs text-zinc-500">{t('models.context_length')}</span>
                  <input
                    type="number"
                    min="1"
                    step="1"
                    bind:value={draft.context_length}
                    class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                </label>
                <label class="space-y-1">
                  <span class="text-xs text-zinc-500">{t('models.max_output')}</span>
                  <input
                    type="number"
                    min="1"
                    step="1"
                    bind:value={draft.max_output_tokens}
                    class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                </label>
                <label class="space-y-1">
                  <span class="text-xs text-zinc-500">{t('models.temperature')}</span>
                  <input
                    type="number"
                    min="0"
                    max="2"
                    step="0.1"
                    bind:value={draft.temperature}
                    class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                  <span class="text-[11px] text-zinc-400">{t('models.temperature_hint')}</span>
                </label>
              </div>

              <div class="space-y-1.5">
                <span class="text-xs text-zinc-500">{t('models.capabilities')}</span>
                <div class="flex flex-wrap gap-2">
                  {#each CAPABILITY_FLAGS as flag (flag)}
                    <button
                      type="button"
                      onclick={() => toggleCapability(flag)}
                      class="px-3 py-1.5 rounded-lg text-xs font-mono border transition cursor-pointer
                        {draft.capabilities[flag]
                          ? 'border-indigo-400 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/40'
                          : 'border-zinc-200 dark:border-zinc-700 text-zinc-400'}"
                    >
                      {t(`models.cap_${flag}`)}
                    </button>
                  {/each}
                </div>
              </div>

              <div class="flex items-center justify-end gap-2">
                <button
                  onclick={cancelEdit}
                  class="px-3.5 py-2 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 text-zinc-600 dark:text-zinc-300 transition cursor-pointer"
                >
                  {t('models.cancel')}
                </button>
                <button
                  onclick={saveDraft}
                  disabled={modelsStore.saving}
                  class="px-3.5 py-2 text-sm rounded-lg bg-indigo-600 hover:bg-indigo-700 text-white font-medium flex items-center gap-2 transition cursor-pointer disabled:opacity-50"
                >
                  {#if modelsStore.saving}
                    <RefreshCw class="w-4 h-4 animate-spin" />
                    <span>{t('models.saving')}</span>
                  {:else}
                    <Check class="w-4 h-4" />
                    <span>{t('models.save')}</span>
                  {/if}
                </button>
              </div>
            </div>
          {:else}
            <!-- Catalog row -->
            <div class="flex flex-wrap items-start justify-between gap-4">
              <div class="min-w-0 flex-1">
                <div class="flex items-center gap-2 flex-wrap">
                  <code class="font-mono font-bold text-sm text-zinc-900 dark:text-zinc-100">
                    {reference}
                  </code>
                  {#if isDefault}
                    <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-mono bg-amber-500/10 text-amber-600 dark:text-amber-400 border border-amber-500/20">
                      <Star class="w-3 h-3 fill-amber-500" />
                      {t('models.default_badge')}
                    </span>
                  {/if}
                  <span class="px-2 py-0.5 rounded text-[11px] font-mono border border-zinc-200 dark:border-zinc-700 text-zinc-500">
                    {sourceLabel(spec.source)}
                  </span>
                </div>

                {#if spec.display_name}
                  <p class="text-xs text-zinc-500 mt-1">{spec.display_name}</p>
                {/if}

                <div class="flex flex-wrap items-center gap-x-4 gap-y-1.5 mt-2 text-xs font-mono text-zinc-500">
                  <span>{t('models.context_length')}: {spec.context_length ?? '-'}</span>
                  <span>{t('models.max_output')}: {spec.max_output_tokens ?? '-'}</span>
                  <span>{t('models.temperature')}: {spec.temperature ?? '-'}</span>
                </div>

                <div class="flex flex-wrap items-center gap-1.5 mt-2">
                  {#each CAPABILITY_FLAGS as flag (flag)}
                    <span
                      class="px-2 py-0.5 rounded-md text-[11px] font-mono border
                        {spec.capabilities[flag]
                          ? 'border-indigo-200 dark:border-indigo-800/60 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/40'
                          : 'border-zinc-200 dark:border-zinc-800 text-zinc-300 dark:text-zinc-600'}"
                    >
                      {t(`models.cap_${flag}`)}
                    </span>
                  {/each}
                </div>
              </div>

              <div class="flex items-center gap-2 shrink-0">
                <button
                  onclick={() => startEdit(spec)}
                  class="px-3 py-1.5 text-xs sm:text-sm font-mono bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 rounded-md transition cursor-pointer flex items-center gap-1.5"
                  title={t('models.edit')}
                >
                  <Pencil class="w-3.5 h-3.5" />
                  <span>{t('models.edit')}</span>
                </button>
                <button
                  onclick={() => removeModel(reference)}
                  disabled={modelsStore.saving}
                  class="p-1.5 text-zinc-400 hover:text-rose-500 transition cursor-pointer disabled:opacity-50"
                  title={t('models.delete')}
                >
                  <Trash2 class="w-4 h-4" />
                </button>
              </div>
            </div>
          {/if}
        </div>
      {/each}

      {#if visibleModels.length === 0 && !isAdding}
        <p class="text-sm text-zinc-400 text-center py-8">{t('models.no_match')}</p>
      {/if}

      <!-- Add row -->
      {#if isAdding}
        <div class="bg-white dark:bg-zinc-900 border border-indigo-300 dark:border-indigo-800 ring-1 ring-indigo-200 dark:ring-indigo-900 rounded-xl p-4 shadow-xs space-y-4">
          <div class="flex items-center gap-2">
            <Plus class="w-4 h-4 text-indigo-500" />
            <span class="font-semibold text-sm text-zinc-900 dark:text-zinc-100">{t('models.add')}</span>
          </div>

          <div class="grid grid-cols-1 sm:grid-cols-2 gap-3 text-sm">
            <label class="space-y-1">
              <span class="text-xs text-zinc-500">{t('models.provider')}</span>
              <select
                bind:value={draft.provider}
                class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden cursor-pointer"
              >
                {#each modelsStore.providers as provider (provider)}
                  <option value={provider}>{provider}</option>
                {/each}
              </select>
            </label>
            <label class="space-y-1">
              <span class="text-xs text-zinc-500">{t('models.model_id')}</span>
              <input
                type="text"
                bind:value={draft.model}
                placeholder="deepseek-chat"
                class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
              />
            </label>
          </div>

          <div class="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-3 text-sm">
            <label class="space-y-1">
              <span class="text-xs text-zinc-500">{t('models.display_name')} ({t('models.optional')})</span>
              <input
                type="text"
                bind:value={draft.display_name}
                class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
              />
            </label>
            <label class="space-y-1">
              <span class="text-xs text-zinc-500">{t('models.context_length')}</span>
              <input
                type="number"
                min="1"
                step="1"
                bind:value={draft.context_length}
                class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
              />
            </label>
            <label class="space-y-1">
              <span class="text-xs text-zinc-500">{t('models.max_output')}</span>
              <input
                type="number"
                min="1"
                step="1"
                bind:value={draft.max_output_tokens}
                class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
              />
            </label>
            <label class="space-y-1">
              <span class="text-xs text-zinc-500">{t('models.temperature')}</span>
              <input
                type="number"
                min="0"
                max="2"
                step="0.1"
                bind:value={draft.temperature}
                class="w-full px-3 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
              />
            </label>
          </div>

          <div class="space-y-1.5">
            <span class="text-xs text-zinc-500">{t('models.capabilities')}</span>
            <div class="flex flex-wrap gap-2">
              {#each CAPABILITY_FLAGS as flag (flag)}
                <button
                  type="button"
                  onclick={() => toggleCapability(flag)}
                  class="px-3 py-1.5 rounded-lg text-xs font-mono border transition cursor-pointer
                    {draft.capabilities[flag]
                      ? 'border-indigo-400 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/40'
                      : 'border-zinc-200 dark:border-zinc-700 text-zinc-400'}"
                >
                  {t(`models.cap_${flag}`)}
                </button>
              {/each}
            </div>
          </div>

          {#if draft.provider && draft.model.trim()}
            <p class="text-xs font-mono text-zinc-500">
              <code class="font-bold text-indigo-600 dark:text-indigo-400">
                {draft.provider}/{draft.model.trim()}
              </code>
            </p>
          {/if}

          <div class="flex items-center justify-end gap-2">
            <button
              onclick={cancelEdit}
              class="px-3.5 py-2 text-sm rounded-lg border border-zinc-200 dark:border-zinc-700 text-zinc-600 dark:text-zinc-300 transition cursor-pointer flex items-center gap-1.5"
            >
              <X class="w-4 h-4" />
              {t('models.cancel')}
            </button>
            <button
              onclick={saveDraft}
              disabled={!draft.provider.trim() || !draft.model.trim() || modelsStore.saving}
              class="px-3.5 py-2 text-sm rounded-lg bg-indigo-600 hover:bg-indigo-700 text-white font-medium flex items-center gap-2 transition cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {#if modelsStore.saving}
                <RefreshCw class="w-4 h-4 animate-spin" />
                <span>{t('models.saving')}</span>
              {:else}
                <Check class="w-4 h-4" />
                <span>{t('models.save')}</span>
              {/if}
            </button>
          </div>
        </div>
      {/if}
    </div>
  {/if}
</div>
