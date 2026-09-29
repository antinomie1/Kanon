<script lang="ts">
import {
  AlertCircle,
  Check,
  CheckCircle2,
  Eye,
  EyeOff,
  Gauge,
  Layers,
  Plus,
  RefreshCw,
  Server,
  Sparkles,
  Star,
  Trash2,
} from 'lucide-svelte';
import { untrack } from 'svelte';
import { t } from '../../stores/i18n.svelte';
import { CAPABILITY_FLAGS, modelsStore } from '../../stores/models.svelte';
import { providersStore } from '../../stores/providers.svelte';
import type {
  ModelSpec,
  ProviderPreset,
  TestProviderRequest,
  UpsertProviderRequest,
} from '../../types';
import ModelsView from './ModelsView.svelte';

/** Protocols offered for a new endpoint, with the base URL used when the field is left empty. */
const protocolDefaults: Record<string, string> = {
  openai: 'https://api.openai.com/v1',
  openai_responses: 'https://api.openai.com/v1',
  anthropic: 'https://api.anthropic.com/v1',
};

/** Draft of the "add provider" modal. */
let providerDraft = $state({
  name: '',
  protocol: 'openai',
  base_url: '',
  api_key: '',
  temperature: '',
  max_tokens: '',
});
let isAddProviderOpen = $state(false);
let isQuickConfigOpen = $state(false);
let createProviderError = $state<string | null>(null);

// Editor state for the selected endpoint. The stored credential is never returned by the API, so
// `editApiKey` always starts blank and omitting it keeps whatever the node already holds.
let editProtocol = $state('openai');
let editBaseUrl = $state('');
let editApiKey = $state('');
let editTemperature = $state('');
let editMaxTokens = $state('');
let clearApiKey = $state(false);
let showApiKey = $state(false);
/** Upstream model id used by the connectivity test (no provider prefix). */
let editTestModel = $state('');
let saveNotification = $state(false);
let discoveryMessage = $state<string | null>(null);

/** Models of one endpoint by their upstream id, which is what a connectivity probe sends. */
function upstreamModelsFor(provider: string): string[] {
  return modelsStore.models
    .filter((spec) => spec.provider === provider)
    .map((spec) => spec.model);
}

/**
 * Catalog models grouped by provider for the default-model picker.
 *
 * The current default is always listed, even when the catalog no longer describes it, so the
 * picker never shows an empty selection for a model the node is actually using.
 */
let modelGroups = $derived.by(() => {
  const groups = new Map<string, string[]>();
  for (const spec of modelsStore.models) {
    groups.set(spec.provider, [
      ...(groups.get(spec.provider) ?? []),
      modelsStore.referenceOf(spec),
    ]);
  }
  const current = modelsStore.defaultModel;
  if (current && !modelsStore.defaultSpec) {
    const provider = current.split('/')[0];
    groups.set(provider, [...(groups.get(provider) ?? []), current]);
  }
  return [...groups.entries()].map(([provider, references]) => ({
    provider,
    references,
  }));
});

/** Text after the provider prefix, which is the model as the endpoint knows it. */
function modelLabel(reference: string): string {
  const slash = reference.indexOf('/');
  return slash === -1 ? reference : reference.slice(slash + 1);
}

/** Whether an endpoint serves the global default model (derived, not a setting of its own). */
function servesDefault(provider: string): boolean {
  return modelsStore.defaultModel?.startsWith(`${provider}/`) ?? false;
}

/**
 * Loads the editor from the selected endpoint.
 *
 * Only the selection itself is tracked: the rest is read untracked so a catalog or model refresh
 * (which this effect would otherwise observe) cannot silently discard values the operator is
 * still typing.
 */
$effect(() => {
  const provider = providersStore.selectedProvider;
  const name = provider?.name ?? '';
  untrack(() => {
    if (!provider) return;
    editProtocol = provider.protocol;
    editBaseUrl = provider.base_url;
    editApiKey = '';
    editTemperature = provider.temperature?.toString() ?? '';
    editMaxTokens = provider.max_tokens?.toString() ?? '';
    clearApiKey = false;
    editTestModel = upstreamModelsFor(name)[0] ?? '';
    showApiKey = false;
    discoveryMessage = null;
  });
});

/** Optional numeric input converted to the wire form; blank means "not configured". */
function optionalNumber(raw: string): number | undefined {
  const value = Number(raw.trim());
  return raw.trim() !== '' && Number.isFinite(value) ? value : undefined;
}

function openAddProvider() {
  providerDraft = {
    name: '',
    protocol: 'openai',
    base_url: '',
    api_key: '',
    temperature: '',
    max_tokens: '',
  };
  createProviderError = null;
  isAddProviderOpen = true;
}

/** Preset selection only prefills the create form; nothing is written until it is submitted. */
function applyPreset(preset: ProviderPreset) {
  providerDraft = {
    name: preset.id,
    protocol: preset.protocol,
    base_url: preset.base_url,
    api_key: '',
    temperature: '',
    max_tokens: '',
  };
  createProviderError = null;
  isQuickConfigOpen = false;
  isAddProviderOpen = true;
}

async function handleCreateProvider() {
  if (!providerDraft.name.trim()) return;
  const req: UpsertProviderRequest = {
    name: providerDraft.name.trim(),
    protocol: providerDraft.protocol,
    base_url:
      providerDraft.base_url.trim() ||
      protocolDefaults[providerDraft.protocol] ||
      '',
    temperature: optionalNumber(providerDraft.temperature),
    max_tokens: optionalNumber(providerDraft.max_tokens),
  };
  // An omitted credential keeps the stored one; on first creation there is simply none.
  if (providerDraft.api_key.trim()) req.api_key = providerDraft.api_key.trim();

  const ok = await providersStore.upsertProvider(req);
  if (ok) {
    isAddProviderOpen = false;
  } else {
    createProviderError = providersStore.actionError;
  }
}

async function handleSaveProvider() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;

  const req: UpsertProviderRequest = {
    name: provider.name,
    protocol: editProtocol,
    base_url: editBaseUrl.trim() || provider.base_url,
    temperature: optionalNumber(editTemperature),
    max_tokens: optionalNumber(editMaxTokens),
  };
  if (editApiKey.trim()) req.api_key = editApiKey.trim();
  if (clearApiKey) req.clear_api_key = true;

  const ok = await providersStore.upsertProvider(req);
  if (ok) {
    editApiKey = '';
    clearApiKey = false;
    saveNotification = true;
    setTimeout(() => {
      saveNotification = false;
    }, 2000);
  }
}

async function handleDeleteProvider() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;
  const message = servesDefault(provider.name)
    ? `${t('providers.delete_confirm')}\n\n${t('providers.delete_default_warning')}`
    : t('providers.delete_confirm');
  if (!confirm(message)) return;
  await providersStore.deleteProvider(provider.name);
}

async function handleDiscover() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;
  discoveryMessage = null;
  const res = await providersStore.discoverModels(provider.name);
  if (res) {
    discoveryMessage = t('providers.discover_done', {
      count: res.discovered.length,
      persisted: res.persisted,
    });
    if (!editTestModel)
      editTestModel = upstreamModelsFor(provider.name)[0] ?? '';
  }
}

/**
 * Probes the selected endpoint.
 *
 * The request names the provider, so the node supplies the stored credential itself. The form's
 * protocol, URL and any typed key are sent as overrides, which lets an operator verify an edit
 * before saving it.
 */
async function handleTestProvider() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;

  const req: TestProviderRequest = {
    prompt: 'ping',
    protocol: editProtocol,
    base_url: editBaseUrl.trim() || undefined,
  };
  if (editApiKey.trim()) req.api_key = editApiKey.trim();
  if (editTestModel.trim()) req.model = editTestModel.trim();

  await providersStore.testProvider(provider.name, req);
}

/** Applies the picker's choice as the one global default model; blank clears it. */
async function handleDefaultChange(
  event: Event & { currentTarget: HTMLSelectElement },
) {
  const select = event.currentTarget;
  const ok = await modelsStore.setDefault(select.value || null);
  // On refusal the node keeps its previous default: put the picker back on it.
  if (!ok) select.value = modelsStore.defaultModel ?? '';
}
</script>

<div class="p-6 space-y-6 max-w-7xl mx-auto font-sans">
  <!-- The one global default model: what answers unless an instance picks its own. -->
  {#if providersStore.providers.length > 0}
    {@const defaultSpec = modelsStore.defaultSpec}
    <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-4 sm:p-5 shadow-xs space-y-4">
      <div class="flex flex-wrap items-start justify-between gap-4">
        <div class="flex items-start gap-3.5">
          <div
            class="p-2.5 rounded-xl {modelsStore.defaultModel
              ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
              : 'bg-amber-500/10 text-amber-600 dark:text-amber-400'}"
          >
            <Star class="w-6 h-6" />
          </div>
          <div class="min-w-0">
            <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('providers.default_model_title')}</h3>
            <p class="text-xs text-zinc-500 mt-1 max-w-xl leading-relaxed">{t('providers.default_model_desc')}</p>
          </div>
        </div>

        <div class="w-full sm:w-96">
          <label for="global-default-model" class="sr-only">{t('providers.default_model_title')}</label>
          <select
            id="global-default-model"
            value={modelsStore.defaultModel ?? ''}
            onchange={handleDefaultChange}
            disabled={modelsStore.saving}
            class="w-full px-3.5 py-2.5 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden cursor-pointer disabled:opacity-60
              {modelsStore.defaultModel ? 'border-emerald-300 dark:border-emerald-800/70' : 'border-amber-300 dark:border-amber-700/70'}"
          >
            <option value="">{t('providers.default_model_unset')}</option>
            {#each modelGroups as group (group.provider)}
              <optgroup label={group.provider}>
                {#each group.references as reference (reference)}
                  <option value={reference}>{modelLabel(reference)}</option>
                {/each}
              </optgroup>
            {/each}
          </select>
        </div>
      </div>

      {#if modelsStore.defaultModel}
        <div class="flex flex-wrap items-center gap-2 text-xs font-mono text-zinc-500">
          <code class="px-2.5 py-1 rounded-lg font-bold text-sm bg-emerald-50 dark:bg-emerald-950/80 text-emerald-600 dark:text-emerald-400 border border-emerald-200 dark:border-emerald-800/60">
            {modelsStore.defaultModel}
          </code>
          {#if defaultSpec?.context_length}
            <span>· {t('providers.context_length')}: {defaultSpec.context_length}</span>
          {/if}
          {#each CAPABILITY_FLAGS as flag (flag)}
            {#if defaultSpec?.capabilities[flag]}
              <span class="px-2 py-0.5 rounded-md text-[11px] border border-indigo-200 dark:border-indigo-800/60 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/40">
                {t(`models.cap_${flag}`)}
              </span>
            {/if}
          {/each}
        </div>
      {:else}
        <p class="text-xs text-amber-600 dark:text-amber-400 flex items-center gap-1.5">
          <AlertCircle class="w-3.5 h-3.5 shrink-0" />
          {modelGroups.length === 0 ? t('providers.default_model_no_models') : t('providers.default_model_none')}
        </p>
      {/if}

      {#if modelsStore.error}
        <p class="text-xs text-rose-600 dark:text-rose-400 flex items-center gap-1.5">
          <AlertCircle class="w-3.5 h-3.5 shrink-0" /> {modelsStore.error}
        </p>
      {/if}
    </div>
  {/if}

  {#if providersStore.catalog === null && providersStore.loading}
    <p class="text-sm text-zinc-400">{t('common.loading')}</p>
  {:else if providersStore.providers.length === 0}
    <!-- Empty state with the two entry points that can create the first endpoint. -->
    <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-2xl p-12 text-center shadow-xs space-y-6">
      <div class="w-16 h-16 rounded-2xl bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 flex items-center justify-center mx-auto text-zinc-400">
        <Server class="w-8 h-8 stroke-[1.5]" />
      </div>
      <div class="max-w-lg mx-auto space-y-2">
        <h3 class="text-lg font-semibold text-zinc-900 dark:text-zinc-100">{t('providers.empty_title')}</h3>
        <p class="text-sm text-zinc-500 leading-relaxed">{t('providers.empty_hint')}</p>
      </div>
      <div class="flex flex-wrap items-center justify-center gap-3.5 pt-2">
        <button
          onclick={() => (isQuickConfigOpen = true)}
          class="px-5 py-2.5 bg-indigo-600 hover:bg-indigo-700 text-white rounded-xl text-sm font-medium flex items-center gap-2 transition cursor-pointer shadow-xs"
        >
          <Sparkles class="w-4.5 h-4.5" />
          <span>{t('providers.quick_config')}</span>
        </button>
        <button
          onclick={openAddProvider}
          class="px-5 py-2.5 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-800 dark:text-zinc-200 rounded-xl text-sm font-medium flex items-center gap-2 transition cursor-pointer"
        >
          <Plus class="w-4.5 h-4.5" />
          <span>{t('providers.add_provider')}</span>
        </button>
      </div>
    </div>
  {:else}
    <div class="grid grid-cols-1 lg:grid-cols-12 gap-6 items-start">
      <!-- Provider directory -->
      <div class="lg:col-span-4 space-y-3">
        <div class="flex items-center justify-between pb-1">
          <div class="flex items-center gap-2">
            <Server class="w-4.5 h-4.5 text-zinc-500" />
            <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('providers.directory_title')}</h3>
          </div>
          <div class="flex items-center gap-2">
            <button
              onclick={() => (isQuickConfigOpen = true)}
              class="px-2.5 py-1.5 text-zinc-600 dark:text-zinc-300 hover:text-indigo-600 dark:hover:text-indigo-400 rounded-lg text-xs sm:text-sm font-medium flex items-center gap-1.5 transition cursor-pointer"
              title={t('providers.quick_config_hint')}
            >
              <Sparkles class="w-4 h-4" />
              <span>{t('providers.quick_config')}</span>
            </button>
            <button
              onclick={openAddProvider}
              class="px-3 py-1.5 bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 rounded-lg text-xs sm:text-sm font-medium flex items-center gap-1.5 transition cursor-pointer shadow-2xs"
            >
              <Plus class="w-4 h-4" />
              <span>{t('providers.add_provider')}</span>
            </button>
          </div>
        </div>

        <div class="space-y-2.5">
          {#each providersStore.providers as prov (prov.name)}
            <div
              onclick={() => providersStore.selectProvider(prov.name)}
              onkeydown={(e) => {
                if (e.key === 'Enter' || e.key === ' ') providersStore.selectProvider(prov.name);
              }}
              role="button"
              tabindex="0"
              class="w-full text-left p-4 rounded-xl border transition cursor-pointer flex items-center justify-between
                {providersStore.selectedProvider?.name === prov.name
                  ? 'bg-zinc-100/90 dark:bg-zinc-800/90 border-zinc-300 dark:border-zinc-700 shadow-xs ring-1 ring-zinc-400 dark:ring-zinc-600'
                  : 'bg-white dark:bg-zinc-900 border-zinc-200 dark:border-zinc-800 hover:border-zinc-300 dark:hover:border-zinc-700'}"
            >
              <div class="min-w-0">
                <div class="flex items-center gap-2 flex-wrap">
                  <span class="font-mono font-bold text-sm text-zinc-900 dark:text-zinc-100">{prov.name}</span>
                  <span class="text-xs font-mono px-2 py-0.5 rounded bg-zinc-200/70 dark:bg-zinc-800 text-zinc-600 dark:text-zinc-400">
                    {prov.protocol}
                  </span>
                  {#if servesDefault(prov.name)}
                    <span title={t('providers.serves_default')} class="inline-flex text-emerald-500">
                      <Star class="w-3.5 h-3.5 fill-emerald-500" />
                    </span>
                  {/if}
                </div>
                <p class="text-xs text-zinc-400 font-mono truncate max-w-[220px] mt-1.5" title={prov.base_url}>
                  {prov.base_url}
                </p>
              </div>
              <div class="text-right shrink-0">
                <span class="inline-flex items-center px-2 py-0.5 rounded text-xs font-mono font-medium bg-zinc-100 dark:bg-zinc-800 text-zinc-500">
                  {t('providers.models_in_catalog', { count: providersStore.referencesFor(prov.name).length })}
                </span>
              </div>
            </div>
          {/each}
        </div>
      </div>

      <!-- Selected endpoint editor -->
      <div class="lg:col-span-8 space-y-6">
        {#if providersStore.selectedProvider}
          {@const prov = providersStore.selectedProvider}
          {@const testResult = providersStore.providerTestResults[prov.name]}

          <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-5 sm:p-6 shadow-xs space-y-4">
            <div class="flex flex-wrap items-center justify-between gap-3 pb-3.5 border-b border-zinc-100 dark:border-zinc-800">
              <div>
                <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100 flex items-center gap-2">
                  <span>{t('providers.directory_title')}:</span>
                  <code class="font-mono text-indigo-600 dark:text-indigo-400 font-bold text-base">{prov.name}</code>
                </h3>
                <p class="text-xs sm:text-sm text-zinc-500 font-mono mt-1">
                  {prov.name}/&lt;model-id&gt;
                </p>
              </div>

              <div class="flex items-center gap-2.5">
                {#if saveNotification}
                  <span class="text-xs sm:text-sm text-emerald-500 font-mono flex items-center gap-1">
                    <Check class="w-4 h-4" />
                    {t('providers.saved')}
                  </span>
                {/if}
                <button
                  onclick={handleSaveProvider}
                  disabled={providersStore.pending}
                  class="px-3.5 py-2 bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 rounded-lg text-sm font-medium transition cursor-pointer disabled:opacity-50"
                >
                  {t('providers.save')}
                </button>
                <button
                  onclick={handleDeleteProvider}
                  disabled={providersStore.pending}
                  class="p-2 text-zinc-400 hover:text-rose-500 transition cursor-pointer disabled:opacity-50"
                  title={t('providers.delete')}
                >
                  <Trash2 class="w-4.5 h-4.5" />
                </button>
              </div>
            </div>

            <div class="grid grid-cols-1 sm:grid-cols-2 gap-4 font-mono">
              <div>
                <label for="prov-protocol-select" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.protocol')}:</label>
                <select
                  id="prov-protocol-select"
                  bind:value={editProtocol}
                  class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden cursor-pointer"
                >
                  {#each providersStore.catalog?.available_protocols ?? [] as proto (proto.id)}
                    <option value={proto.id}>{proto.name}</option>
                  {/each}
                </select>
              </div>

              <div>
                <label for="prov-base-url-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.base_url')}:</label>
                <input
                  id="prov-base-url-input"
                  type="text"
                  bind:value={editBaseUrl}
                  placeholder="https://api.deepseek.com/v1"
                  class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                />
              </div>

              <div>
                <label for="prov-temperature-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.temperature')}:</label>
                <input
                  id="prov-temperature-input"
                  type="number"
                  min="0"
                  max="2"
                  step="0.1"
                  bind:value={editTemperature}
                  placeholder="0.7"
                  class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                />
                <span class="text-xs text-zinc-400 mt-1 block font-sans">{t('providers.temperature_hint')}</span>
              </div>

              <div>
                <label for="prov-max-tokens-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.max_tokens')}:</label>
                <input
                  id="prov-max-tokens-input"
                  type="number"
                  min="1"
                  step="1"
                  bind:value={editMaxTokens}
                  placeholder="4096"
                  class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                />
                <span class="text-xs text-zinc-400 mt-1 block font-sans">{t('providers.max_tokens_hint')}</span>
              </div>

              <div class="sm:col-span-2">
                <label for="prov-api-key-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.api_key')}:</label>
                <div class="relative">
                  <input
                    id="prov-api-key-input"
                    type={showApiKey ? 'text' : 'password'}
                    bind:value={editApiKey}
                    placeholder={prov.api_key_configured ? t('providers.api_key_keep') : 'sk-...'}
                    class="w-full px-3.5 py-2 pr-10 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                  <button
                    type="button"
                    onclick={() => (showApiKey = !showApiKey)}
                    class="absolute right-3 top-2.5 text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 cursor-pointer"
                    title={showApiKey ? t('adapters.milky_token_hide') : t('adapters.milky_token_show')}
                  >
                    {#if showApiKey}
                      <EyeOff class="w-4 h-4" />
                    {:else}
                      <Eye class="w-4 h-4" />
                    {/if}
                  </button>
                </div>
                <div class="flex flex-wrap items-center gap-3 mt-1.5">
                  <span class="text-xs {prov.api_key_configured ? 'text-emerald-500' : 'text-zinc-400'} font-sans">
                    {prov.api_key_configured ? t('providers.api_key_configured') : t('providers.api_key_unset')}
                  </span>
                  {#if prov.api_key_configured}
                    <label class="flex items-center gap-1.5 text-xs text-zinc-500 font-sans cursor-pointer">
                      <input type="checkbox" bind:checked={clearApiKey} class="rounded text-rose-600" />
                      <span>{t('providers.api_key_clear')}</span>
                    </label>
                  {/if}
                </div>
                <span class="text-xs text-zinc-400 mt-1.5 block font-sans">{t('providers.api_key_hint')}</span>
              </div>
            </div>
          </div>

          <!-- Connectivity + model discovery -->
          <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-5 shadow-xs space-y-4">
            <div class="flex items-center gap-2 pb-3 border-b border-zinc-100 dark:border-zinc-800">
              <Gauge class="w-4.5 h-4.5 text-indigo-500" />
              <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('providers.test_title')}</h3>
            </div>

            <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <div class="space-y-3">
                <div>
                  <label for="prov-test-model-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.test_model_label')}:</label>
                  <input
                    id="prov-test-model-input"
                    type="text"
                    list="prov-test-model-options"
                    bind:value={editTestModel}
                    placeholder={t('providers.test_model_placeholder')}
                    class="w-full px-3.5 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                  <datalist id="prov-test-model-options">
                    {#each upstreamModelsFor(prov.name) as model (model)}
                      <option value={model}></option>
                    {/each}
                  </datalist>
                </div>
                <p class="text-xs text-zinc-400 leading-relaxed">{t('providers.test_key_hint')}</p>
              </div>

              <div class="space-y-3">
                <div class="flex flex-wrap items-center gap-2">
                  <button
                    onclick={handleTestProvider}
                    disabled={providersStore.testingProvider === prov.name}
                    class="px-3.5 py-2 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer disabled:opacity-50"
                  >
                    {#if providersStore.testingProvider === prov.name}
                      <RefreshCw class="w-4 h-4 animate-spin" />
                      <span>{t('providers.testing')}...</span>
                    {:else}
                      <Gauge class="w-4 h-4" />
                      <span>{t('providers.test')}</span>
                    {/if}
                  </button>

                  <button
                    onclick={handleDiscover}
                    disabled={modelsStore.saving}
                    class="px-3.5 py-2 bg-indigo-50 dark:bg-indigo-950/60 hover:bg-indigo-100 dark:hover:bg-indigo-900/60 text-indigo-600 dark:text-indigo-400 border border-indigo-200 dark:border-indigo-800/80 rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer disabled:opacity-50"
                    title={t('providers.discover_hint')}
                  >
                    {#if modelsStore.saving}
                      <RefreshCw class="w-3.5 h-3.5 animate-spin" />
                      <span>{t('providers.discovering')}</span>
                    {:else}
                      <Layers class="w-4 h-4" />
                      <span>{t('providers.discover')}</span>
                    {/if}
                  </button>
                </div>

                {#if testResult}
                  <p class="text-xs font-mono flex items-start gap-1.5 {testResult.status === 'ok' ? 'text-emerald-600 dark:text-emerald-400' : 'text-rose-600 dark:text-rose-400'}">
                    {#if testResult.status === 'ok'}
                      <CheckCircle2 class="w-3.5 h-3.5 shrink-0 mt-0.5" />
                      <span>{testResult.latency_ms}ms · {testResult.model}</span>
                    {:else}
                      <AlertCircle class="w-3.5 h-3.5 shrink-0 mt-0.5" />
                      <span>{testResult.error}</span>
                    {/if}
                  </p>
                {/if}
                {#if discoveryMessage}
                  <p class="text-xs font-mono text-indigo-600 dark:text-indigo-400 flex items-center gap-1.5">
                    <CheckCircle2 class="w-3.5 h-3.5" />
                    <span>{discoveryMessage}</span>
                  </p>
                {/if}
              </div>
            </div>
          </div>

          <!-- This endpoint's own model catalog: capabilities, context window and modalities. -->
          <ModelsView provider={prov.name} />
        {/if}
      </div>
    </div>
  {/if}
</div>

<!-- Modal: provider templates -->
{#if isQuickConfigOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (isQuickConfigOpen = false)}
    role="button"
    tabindex="-1"
  >
    <div
      class="w-full max-w-lg bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between pb-3 border-b border-zinc-100 dark:border-zinc-800">
        <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100 flex items-center gap-2">
          <Sparkles class="w-4.5 h-4.5 text-indigo-500" />
          <span>{t('providers.quick_config_title')}</span>
        </h3>
        <button
          onclick={() => (isQuickConfigOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-sm cursor-pointer"
        >
          ✕
        </button>
      </div>

      <div class="space-y-3 text-sm">
        <p class="text-zinc-500 leading-relaxed">{t('providers.quick_config_hint')}</p>

        <div class="grid grid-cols-1 sm:grid-cols-2 gap-3 pt-1">
          {#each providersStore.catalog?.presets ?? [] as preset (preset.id)}
            <button
              onclick={() => applyPreset(preset)}
              class="p-3.5 text-left rounded-xl border border-zinc-200 dark:border-zinc-800 bg-zinc-50 dark:bg-zinc-950/60 hover:border-indigo-400 dark:hover:border-indigo-600 hover:bg-white dark:hover:bg-zinc-900 transition cursor-pointer flex flex-col justify-between group shadow-2xs"
            >
              <div>
                <div class="flex items-center justify-between">
                  <span class="font-bold text-sm text-zinc-900 dark:text-zinc-100 group-hover:text-indigo-600 dark:group-hover:text-indigo-400">
                    {preset.name}
                  </span>
                  <span class="text-xs font-mono px-1.5 py-0.5 rounded bg-zinc-200/70 dark:bg-zinc-800 text-zinc-500">
                    {preset.protocol}
                  </span>
                </div>
                <span class="text-xs font-mono text-zinc-400 block truncate mt-1" title={preset.base_url}>
                  {preset.base_url}
                </span>
              </div>
            </button>
          {/each}
        </div>
      </div>
    </div>
  </div>
{/if}

<!-- Modal: add provider (also reached from a template) -->
{#if isAddProviderOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (isAddProviderOpen = false)}
    role="button"
    tabindex="-1"
  >
    <div
      class="w-full max-w-md bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between pb-3 border-b border-zinc-100 dark:border-zinc-800">
        <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100 flex items-center gap-2">
          <Plus class="w-4.5 h-4.5 text-indigo-500" />
          <span>{t('providers.manual_add_title')}</span>
        </h3>
        <button
          onclick={() => (isAddProviderOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-sm cursor-pointer"
        >
          ✕
        </button>
      </div>

      <div class="space-y-3.5 text-sm font-mono">
        <div>
          <label for="new-prov-name" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.name')}:</label>
          <input
            id="new-prov-name"
            type="text"
            bind:value={providerDraft.name}
            placeholder="deepseek"
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
          <p class="text-xs font-sans text-zinc-400 mt-1">{t('providers.name_hint')}</p>
        </div>

        <div>
          <label for="new-prov-protocol" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.protocol')}:</label>
          <select
            id="new-prov-protocol"
            bind:value={providerDraft.protocol}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden cursor-pointer"
          >
            {#each providersStore.catalog?.available_protocols ?? [] as proto (proto.id)}
              <option value={proto.id}>{proto.name}</option>
            {/each}
          </select>
        </div>

        <div>
          <label for="new-prov-base-url" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.base_url')}:</label>
          <input
            id="new-prov-base-url"
            type="text"
            bind:value={providerDraft.base_url}
            placeholder={protocolDefaults[providerDraft.protocol] ?? ''}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
        </div>

        <div>
          <label for="new-prov-api-key" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.api_key')}:</label>
          <input
            id="new-prov-api-key"
            type="password"
            bind:value={providerDraft.api_key}
            placeholder="sk-..."
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
        </div>

        <div class="grid grid-cols-2 gap-3">
          <div>
            <label for="new-prov-temperature" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.temperature')}:</label>
            <input
              id="new-prov-temperature"
              type="number"
              min="0"
              max="2"
              step="0.1"
              bind:value={providerDraft.temperature}
              class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
            />
          </div>
          <div>
            <label for="new-prov-max-tokens" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.max_tokens')}:</label>
            <input
              id="new-prov-max-tokens"
              type="number"
              min="1"
              step="1"
              bind:value={providerDraft.max_tokens}
              class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
            />
          </div>
        </div>

        {#if createProviderError}
          <p class="text-xs font-sans text-rose-600 dark:text-rose-400 flex items-center gap-1.5">
            <AlertCircle class="w-3.5 h-3.5" /> {createProviderError}
          </p>
        {/if}
      </div>

      <div class="pt-3 border-t border-zinc-100 dark:border-zinc-800 flex items-center justify-end gap-2">
        <button
          onclick={() => (isAddProviderOpen = false)}
          class="px-3.5 py-2 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 rounded-lg text-sm font-medium cursor-pointer"
        >
          {t('common.cancel')}
        </button>
        <button
          onclick={handleCreateProvider}
          disabled={!providerDraft.name.trim() || providersStore.pending}
          class="px-4 py-2 bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 rounded-lg text-sm font-medium cursor-pointer disabled:opacity-50"
        >
          {t('providers.create_provider')}
        </button>
      </div>
    </div>
  </div>
{/if}
