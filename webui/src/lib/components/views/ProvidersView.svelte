<script lang="ts">
import {
  AlertCircle,
  Check,
  CheckCircle2,
  Cpu,
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
  Zap,
} from 'lucide-svelte';
import { untrack } from 'svelte';
import { t } from '../../stores/i18n.svelte';
import { CAPABILITY_FLAGS, modelsStore } from '../../stores/models.svelte';
import { providersStore } from '../../stores/providers.svelte';
import type {
  ProviderPreset,
  TestProviderRequest,
  UpsertProviderRequest,
} from '../../types';

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

/** Draft of the legacy "create default provider" flow (`PUT /providers/active`). */
let defaultDraft = $state({
  protocol: 'openai',
  base_url: '',
  model: '',
  api_key: '',
  temperature: '',
  max_tokens: '',
  provider_name: '',
});
let isCreateDefaultOpen = $state(false);

// Editor state for the selected endpoint. The stored credential is never returned by the API, so
// `editApiKey` always starts blank and omitting it keeps whatever the node already holds.
let editProtocol = $state('openai');
let editBaseUrl = $state('');
let editApiKey = $state('');
let editTemperature = $state('');
let editMaxTokens = $state('');
let clearApiKey = $state(false);
let showApiKey = $state(false);
let editModelRef = $state('');
let editTestModel = $state('');
let saveNotification = $state(false);
let discoveryMessage = $state<string | null>(null);

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
    editModelRef = modelsStore.referencesFor(name)[0] ?? '';
    editTestModel = modelsStore.referencesFor(name)[0] ?? '';
    showApiKey = false;
    discoveryMessage = null;
  });
});

/** Optional numeric input converted to the wire form; blank means "not configured". */
function optionalNumber(raw: string): number | undefined {
  const value = Number(raw.trim());
  return raw.trim() !== '' && Number.isFinite(value) ? value : undefined;
}

/**
 * Strips the provider prefix from a reference.
 *
 * The test endpoint sends the model tag upstream, so a canonical `provider/model` reference must
 * lose its prefix; doing it here keeps the same rule for the reference and the probe.
 */
function upstreamModel(reference: string): string {
  const slash = reference.indexOf('/');
  return slash === -1 ? reference : reference.slice(slash + 1);
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
    createProviderError = providersStore.nodeError;
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
    saveNotification = true;
    setTimeout(() => {
      saveNotification = false;
    }, 2000);
  }
}

async function handleDeleteProvider() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;
  if (!confirm(t('providers.delete_confirm'))) return;
  await providersStore.deleteProvider(provider.name);
}

async function handleSetDefault() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;
  await providersStore.setDefaultProvider({
    provider: provider.name,
    model: editModelRef.trim() || undefined,
  });
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
  }
}

/**
 * Probes the selected endpoint.
 *
 * The endpoint the node already answers with is tested without coordinates so the server reuses
 * its own stored credential; every other endpoint is probed with what the form holds, because the
 * browser never receives the stored secret.
 */
async function handleTestProvider() {
  const provider = providersStore.selectedProvider;
  if (!provider) return;

  const reference = editTestModel.trim() || editModelRef.trim();
  const isEffective = providersStore.catalog?.active.provider === provider.name;
  const req: TestProviderRequest = { prompt: 'ping' };
  if (reference) req.model = upstreamModel(reference);

  if (!(isEffective && !editApiKey.trim())) {
    req.protocol = editProtocol;
    req.base_url = editBaseUrl.trim() || undefined;
    if (editApiKey.trim()) req.api_key = editApiKey.trim();
  }

  await providersStore.testProvider(provider.name, req);
}

async function handleCreateDefault() {
  if (!defaultDraft.model.trim() || !defaultDraft.base_url.trim()) return;
  const res = await providersStore.activateOnNode({
    protocol: defaultDraft.protocol,
    base_url: defaultDraft.base_url.trim(),
    model: defaultDraft.model.trim(),
    api_key: defaultDraft.api_key.trim() || undefined,
    temperature: optionalNumber(defaultDraft.temperature),
    max_tokens: optionalNumber(defaultDraft.max_tokens),
    provider_name: defaultDraft.provider_name.trim() || undefined,
  });
  if (res) isCreateDefaultOpen = false;
}

async function handleClearAll() {
  if (!confirm(t('providers.clear_confirm'))) return;
  await providersStore.clearOnNode();
}
</script>

<div class="p-6 space-y-6 max-w-7xl mx-auto font-sans">
  <!-- Effective node provider: this is what actually decides whether the bot replies. -->
  <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-4 sm:p-5 shadow-xs space-y-4">
    <div class="flex flex-wrap items-start justify-between gap-4">
      <div class="flex items-start gap-3.5">
        <div
          class="p-2.5 rounded-xl {providersStore.nodeProvider?.configured
            ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
            : 'bg-amber-500/10 text-amber-600 dark:text-amber-400'}"
        >
          <Cpu class="w-6 h-6" />
        </div>
        <div>
          <div class="flex items-center gap-2.5 flex-wrap">
            <span class="text-sm text-zinc-500 font-medium">{t('providers.node_effective')}</span>
            {#if providersStore.nodeProvider?.configured}
              <code class="px-2.5 py-1 rounded-lg font-mono text-sm font-bold bg-emerald-50 dark:bg-emerald-950/80 text-emerald-600 dark:text-emerald-400 border border-emerald-200 dark:border-emerald-800/60">
                {providersStore.nodeProvider.model}
              </code>
              <span class="px-2 py-0.5 rounded-md text-xs font-mono border border-zinc-200 dark:border-zinc-700 text-zinc-500">
                {t(`providers.source_${providersStore.nodeProvider.source}`)}
              </span>
            {:else}
              <span class="text-sm text-amber-600 dark:text-amber-400 font-medium">
                {t('providers.node_none')}
              </span>
            {/if}
          </div>
          <p class="text-xs text-zinc-400 mt-1 font-mono break-all">
            {providersStore.nodeProvider?.base_url ?? 'base_url: -'}
            {providersStore.nodeProvider?.api_key_configured ? ' · key: ✓' : ' · key: -'}
          </p>
          {#if providersStore.nodeProvider?.configured}
            <div class="flex flex-wrap items-center gap-2 mt-2 text-xs font-mono text-zinc-500">
              {#if providersStore.nodeProvider.provider}
                <span>{t('providers.default_provider')}: {providersStore.nodeProvider.provider}</span>
              {/if}
              <span>· {providersStore.nodeProvider.protocol}</span>
              {#if providersStore.nodeProvider.upstream_model}
                <span>· {t('providers.upstream_model')}: {providersStore.nodeProvider.upstream_model}</span>
              {/if}
              {#if providersStore.nodeProvider.context_length}
                <span>· {t('providers.context_length')}: {providersStore.nodeProvider.context_length}</span>
              {/if}
            </div>
            <div class="flex flex-wrap items-center gap-1.5 mt-2">
              {#each CAPABILITY_FLAGS as flag (flag)}
                {#if providersStore.nodeProvider.capabilities[flag]}
                  <span class="px-2 py-0.5 rounded-md text-[11px] font-mono border border-indigo-200 dark:border-indigo-800/60 text-indigo-600 dark:text-indigo-400 bg-indigo-50 dark:bg-indigo-950/40">
                    {t(`models.cap_${flag}`)}
                  </span>
                {/if}
              {/each}
            </div>
          {/if}
        </div>
      </div>

      <div class="flex items-center gap-2">
        <button
          onclick={() => {
            defaultDraft = {
              protocol: 'openai',
              base_url: '',
              model: '',
              api_key: '',
              temperature: '',
              max_tokens: '',
              provider_name: '',
            };
            isCreateDefaultOpen = true;
          }}
          class="px-3.5 py-2 bg-emerald-600 hover:bg-emerald-700 text-white rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer shadow-2xs"
          title={t('providers.create_default_hint')}
        >
          <Zap class="w-4 h-4" />
          <span>{t('providers.create_default')}</span>
        </button>
        {#if providersStore.providers.length > 0}
          <button
            onclick={handleClearAll}
            disabled={providersStore.nodeActionPending}
            class="px-3 py-2 bg-zinc-100 dark:bg-zinc-800 hover:bg-rose-50 dark:hover:bg-rose-950/30 text-zinc-600 dark:text-zinc-300 hover:text-rose-600 rounded-lg text-sm font-medium transition cursor-pointer disabled:opacity-50"
            title={t('providers.clear_all')}
          >
            <Trash2 class="w-4 h-4" />
          </button>
        {/if}
      </div>
    </div>

    {#if providersStore.nodeMessage}
      <p class="text-xs text-emerald-600 dark:text-emerald-400 flex items-center gap-1.5">
        <CheckCircle2 class="w-3.5 h-3.5" /> {providersStore.nodeMessage}
      </p>
    {/if}
    {#if providersStore.nodeError}
      <p class="text-xs text-rose-600 dark:text-rose-400 flex items-center gap-1.5">
        <AlertCircle class="w-3.5 h-3.5" /> {providersStore.nodeError}
      </p>
    {/if}
    <p class="text-xs text-zinc-400">{t('providers.node_hint')}</p>
  </div>

  <!-- Persisted default: what an unprefixed model reference resolves to. -->
  <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-4 sm:p-5 shadow-xs flex flex-wrap items-center justify-between gap-4">
    <div class="flex items-center gap-3.5">
      <div class="p-2.5 rounded-xl bg-indigo-500/10 text-indigo-600 dark:text-indigo-400">
        <Star class="w-6 h-6" />
      </div>
      <div>
        <div class="flex items-center gap-2.5 flex-wrap">
          <span class="text-sm text-zinc-500 font-medium">{t('providers.default_provider')}:</span>
          {#if providersStore.defaultProvider}
            <code class="px-2.5 py-1 rounded-lg font-mono text-sm font-bold bg-indigo-50 dark:bg-indigo-950/80 text-indigo-600 dark:text-indigo-400 border border-indigo-200 dark:border-indigo-800/60">
              {providersStore.defaultProvider}
            </code>
          {:else}
            <span class="text-sm text-zinc-400 font-mono">{t('providers.no_providers')}</span>
          {/if}
        </div>
        <div class="flex items-center gap-2.5 mt-1.5 flex-wrap">
          <span class="text-xs text-zinc-500">{t('providers.default_model')}:</span>
          {#if providersStore.defaultModel}
            <code class="font-mono text-xs font-semibold text-zinc-700 dark:text-zinc-300">
              {providersStore.defaultModel}
            </code>
          {:else}
            <span class="text-xs text-zinc-400 font-mono">{t('providers.default_model_none')}</span>
          {/if}
        </div>
      </div>
    </div>
  </div>

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
                  {#if prov.is_default}
                    <span class="inline-flex items-center gap-1 px-2 py-0.5 rounded text-xs font-medium font-mono bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border border-emerald-500/20">
                      <Star class="w-3 h-3 fill-emerald-500" />
                      {t('providers.default_badge')}
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
                  disabled={providersStore.nodeActionPending}
                  class="px-3.5 py-2 bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 rounded-lg text-sm font-medium transition cursor-pointer disabled:opacity-50"
                >
                  {t('providers.save')}
                </button>
                <button
                  onclick={handleDeleteProvider}
                  disabled={providersStore.nodeActionPending}
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

          <!-- Default model + discovery + connectivity -->
          <div class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-5 shadow-xs space-y-4">
            <div class="flex items-center gap-2 pb-3 border-b border-zinc-100 dark:border-zinc-800">
              <Star class="w-4.5 h-4.5 text-amber-500" />
              <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('providers.default_model')}</h3>
              {#if prov.is_default}
                <span class="px-2 py-0.5 rounded text-xs font-mono bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border border-emerald-500/20">
                  {t('providers.default_badge')}
                </span>
              {/if}
            </div>

            <div class="grid grid-cols-1 sm:grid-cols-2 gap-4">
              <div>
                <label for="prov-default-model-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.default_model_label')}:</label>
                <input
                  id="prov-default-model-input"
                  type="text"
                  list="prov-default-model-options"
                  bind:value={editModelRef}
                  placeholder={t('providers.default_model_placeholder')}
                  class="w-full px-3.5 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                />
                <datalist id="prov-default-model-options">
                  {#each providersStore.referencesFor(prov.name) as reference (reference)}
                    <option value={reference}></option>
                  {/each}
                </datalist>
                <span class="text-xs text-zinc-400 mt-1 block">{t('providers.default_model_hint')}</span>
                <div class="flex items-center gap-2 mt-3">
                  <button
                    onclick={handleSetDefault}
                    disabled={providersStore.nodeActionPending}
                    class="px-3.5 py-2 bg-indigo-600 hover:bg-indigo-700 text-white rounded-lg text-sm font-medium flex items-center gap-2 transition cursor-pointer disabled:opacity-50"
                  >
                    <Star class="w-4 h-4" />
                    <span>{t('providers.set_default')}</span>
                  </button>
                </div>
              </div>

              <div class="space-y-3">
                <div>
                  <label for="prov-test-model-input" class="block text-xs sm:text-sm font-medium text-zinc-500 mb-1.5">{t('providers.test_model_label')}:</label>
                  <input
                    id="prov-test-model-input"
                    type="text"
                    list="prov-default-model-options"
                    bind:value={editTestModel}
                    placeholder={t('providers.default_model_placeholder')}
                    class="w-full px-3.5 py-2 text-sm font-mono bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
                  />
                </div>
                <p class="text-xs text-zinc-400 leading-relaxed">{t('providers.test_key_hint')}</p>
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
                {#if modelsStore.error}
                  <p class="text-xs font-mono text-rose-600 dark:text-rose-400">{modelsStore.error}</p>
                {/if}
              </div>
            </div>
          </div>
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
          disabled={!providerDraft.name.trim() || providersStore.nodeActionPending}
          class="px-4 py-2 bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-900 hover:bg-zinc-800 dark:hover:bg-zinc-200 rounded-lg text-sm font-medium cursor-pointer disabled:opacity-50"
        >
          {t('providers.create_provider')}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- Modal: legacy activation, surfaced as "create default provider" -->
{#if isCreateDefaultOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (isCreateDefaultOpen = false)}
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
          <Zap class="w-4.5 h-4.5 text-emerald-500" />
          <span>{t('providers.create_default_title')}</span>
        </h3>
        <button
          onclick={() => (isCreateDefaultOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-sm cursor-pointer"
        >
          ✕
        </button>
      </div>

      <p class="text-xs text-zinc-500 leading-relaxed">{t('providers.create_default_hint')}</p>

      <div class="space-y-3.5 text-sm font-mono">
        <div>
          <label for="default-prov-protocol" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.protocol')}:</label>
          <select
            id="default-prov-protocol"
            bind:value={defaultDraft.protocol}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden cursor-pointer"
          >
            {#each providersStore.catalog?.available_protocols ?? [] as proto (proto.id)}
              <option value={proto.id}>{proto.name}</option>
            {/each}
          </select>
        </div>

        <div>
          <label for="default-prov-name" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.name')}:</label>
          <input
            id="default-prov-name"
            type="text"
            bind:value={defaultDraft.provider_name}
            placeholder="deepseek"
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
          <p class="text-xs font-sans text-zinc-400 mt-1">{t('providers.name_hint')}</p>
        </div>

        <div>
          <label for="default-prov-base-url" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.base_url')}:</label>
          <input
            id="default-prov-base-url"
            type="text"
            bind:value={defaultDraft.base_url}
            placeholder={protocolDefaults[defaultDraft.protocol] ?? ''}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
        </div>

        <div>
          <label for="default-prov-model" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.default_model_label')}:</label>
          <input
            id="default-prov-model"
            type="text"
            bind:value={defaultDraft.model}
            placeholder={t('providers.default_model_placeholder')}
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
          <p class="text-xs font-sans text-zinc-400 mt-1">{t('providers.default_model_hint')}</p>
        </div>

        <div>
          <label for="default-prov-api-key" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.api_key')}:</label>
          <input
            id="default-prov-api-key"
            type="password"
            bind:value={defaultDraft.api_key}
            placeholder="sk-..."
            class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
          />
        </div>

        <div class="grid grid-cols-2 gap-3">
          <div>
            <label for="default-prov-temperature" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.temperature')}:</label>
            <input
              id="default-prov-temperature"
              type="number"
              min="0"
              max="2"
              step="0.1"
              bind:value={defaultDraft.temperature}
              class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
            />
          </div>
          <div>
            <label for="default-prov-max-tokens" class="block text-xs font-sans text-zinc-500 mb-1">{t('providers.max_tokens')}:</label>
            <input
              id="default-prov-max-tokens"
              type="number"
              min="1"
              step="1"
              bind:value={defaultDraft.max_tokens}
              class="w-full px-3.5 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg text-zinc-900 dark:text-zinc-100 focus:outline-hidden"
            />
          </div>
        </div>
      </div>

      <div class="pt-3 border-t border-zinc-100 dark:border-zinc-800 flex items-center justify-end gap-2">
        <button
          onclick={() => (isCreateDefaultOpen = false)}
          class="px-3.5 py-2 bg-zinc-100 dark:bg-zinc-800 hover:bg-zinc-200 dark:hover:bg-zinc-700 text-zinc-700 dark:text-zinc-300 rounded-lg text-sm font-medium cursor-pointer"
        >
          {t('common.cancel')}
        </button>
        <button
          onclick={handleCreateDefault}
          disabled={!defaultDraft.model.trim() || !defaultDraft.base_url.trim() || providersStore.nodeActionPending}
          class="px-4 py-2 bg-emerald-600 hover:bg-emerald-700 text-white rounded-lg text-sm font-medium cursor-pointer disabled:opacity-50"
        >
          {t('providers.apply_to_node')}
        </button>
      </div>
    </div>
  </div>
{/if}
