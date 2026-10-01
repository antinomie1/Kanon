<script lang="ts">
import { ChevronDown, PlugZap, Trash2 } from 'lucide-svelte';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { providersStore } from '../../stores/providers.svelte';
import { toasts } from '../../stores/toast.svelte';
import type {
  ProviderInfo,
  TestProviderRequest,
  UpsertProviderRequest,
} from '../../types';
import SecretInput from '../ui/SecretInput.svelte';
import Section from '../ui/Section.svelte';
import Select from '../ui/Select.svelte';
import { optionalNumber } from './modelFormat';

/**
 * Editor of one provider endpoint: where it is, how to reach it, and a probe to check both.
 *
 * Edits collect in a draft and are written together from the save bar, because a URL and a key
 * usually change as a pair and a half-applied pair would only fail. The parent keys this component
 * by provider name, so switching providers always starts from a fresh draft.
 */
let {
  provider,
  dirty = $bindable(false),
  onDeleted,
}: {
  provider: ProviderInfo;
  /** Whether the draft differs from what the node has; read by the page to guard navigation. */
  dirty?: boolean;
  onDeleted: () => void;
} = $props();

// The node never returns the stored credential, so `apiKey` always starts blank and leaving it
// blank keeps whatever the node holds.
let protocol = $state('');
let baseUrl = $state('');
let apiKey = $state('');
let clearKey = $state(false);
let temperature = $state('');
let maxTokens = $state('');
let formError = $state<string | null>(null);

/** Upstream model id used by the probe (no provider prefix). */
let testModel = $state('');
let showAdvanced = $state(false);

/** Puts the draft back to what the node has stored. */
function reset() {
  protocol = provider.protocol;
  baseUrl = provider.base_url;
  apiKey = '';
  clearKey = false;
  temperature = provider.temperature?.toString() ?? '';
  maxTokens = provider.max_tokens?.toString() ?? '';
  formError = null;
}

/** First fill of the form. The component is keyed by provider, so this runs once per provider. */
function init() {
  reset();
  testModel =
    modelsStore.models.find((spec) => spec.provider === provider.name)?.model ??
    '';
  // Open the advanced part straight away when it already holds something.
  showAdvanced = provider.temperature !== null || provider.max_tokens !== null;
}
init();

const protocols = $derived(providersStore.catalog?.available_protocols ?? []);
const protocolInfo = $derived(protocols.find((p) => p.id === protocol));
const upstreamModels = $derived(
  modelsStore.models
    .filter((spec) => spec.provider === provider.name)
    .map((spec) => spec.model),
);

const changeCount = $derived(
  [
    protocol !== provider.protocol,
    baseUrl.trim() !== provider.base_url,
    apiKey.trim() !== '' || clearKey,
    temperature.trim() !== (provider.temperature?.toString() ?? ''),
    maxTokens.trim() !== (provider.max_tokens?.toString() ?? ''),
  ].filter(Boolean).length,
);

$effect(() => {
  dirty = changeCount > 0;
});

const testResult = $derived(providersStore.providerTestResults[provider.name]);
const testing = $derived(providersStore.testingProvider === provider.name);
const servesDefault = $derived(
  modelsStore.defaultModel?.startsWith(`${provider.name}/`) ?? false,
);

async function save() {
  formError = null;
  const temp = optionalNumber(temperature);
  const tokens = optionalNumber(maxTokens);
  if (temp === null || tokens === null) {
    formError = t('llm.bad_number');
    showAdvanced = true;
    return;
  }
  // A blank address means the protocol's usual one, which the placeholder already shows.
  const url = baseUrl.trim() || protocolInfo?.default_base_url || '';
  if (!url) {
    formError = t('llm.url_required');
    return;
  }
  const req: UpsertProviderRequest = {
    name: provider.name,
    protocol,
    base_url: url,
    temperature: temp,
    max_tokens: tokens,
  };
  if (apiKey.trim()) req.api_key = apiKey.trim();
  if (clearKey) req.clear_api_key = true;

  if (await providersStore.upsertProvider(req)) {
    // `provider` now describes what the node stored; start the draft over from it.
    reset();
    toasts.ok(t('llm.saved_toast', { name: provider.name }));
  } else {
    formError = providersStore.actionError;
  }
}

/**
 * Probes the endpoint with a one-word prompt. The request names the provider so the node supplies
 * the stored key itself; whatever the form holds is sent as an override, which lets an edit be
 * checked before it is saved.
 */
async function test() {
  const req: TestProviderRequest = {
    prompt: 'ping',
    protocol,
    base_url: baseUrl.trim() || undefined,
  };
  if (apiKey.trim()) req.api_key = apiKey.trim();
  if (testModel.trim()) req.model = testModel.trim();
  await providersStore.testProvider(provider.name, req);
}

async function remove() {
  const count = upstreamModels.length;
  const yes = await confirmDialog({
    title: t('llm.delete_title', { name: provider.name }),
    message: servesDefault
      ? t('llm.delete_text_default')
      : count === 1
        ? t('llm.delete_text_one')
        : t('llm.delete_text', { n: count }),
    confirm: t('llm.delete'),
    danger: true,
  });
  if (!yes) return;
  const name = provider.name;
  if (await providersStore.deleteProvider(name)) {
    toasts.ok(t('llm.deleted_toast', { name }));
    onDeleted();
  } else {
    toasts.error(providersStore.actionError ?? t('common.error'));
  }
}
</script>

<section class="card relative px-5 pb-6 sm:px-7">
  <div class="flex flex-wrap items-center gap-x-4 gap-y-3 pt-5 pb-2">
    <div class="flex min-w-0 flex-1 flex-col leading-tight">
      <h2 class="m-0 truncate text-[22px] font-extrabold">{provider.name}</h2>
      <span class="mt-1 flex flex-wrap items-center gap-x-3 gap-y-1 text-[13.5px] text-fg2">
        <span>{protocols.find((p) => p.id === provider.protocol)?.name ?? provider.protocol}</span>
        <span class="flex items-center gap-1.5 whitespace-nowrap">
          {#if provider.api_key_configured}
            <i class="dot dot-ok"></i>{t('llm.key_saved')}
          {:else}
            <i class="dot dot-warn"></i>{t('llm.no_key')}
          {/if}
        </span>
      </span>
    </div>
    <button
      type="button"
      class="btn btn-sm btn-icon btn-danger"
      title={t('llm.delete_title', { name: provider.name })}
      aria-label={t('llm.delete_title', { name: provider.name })}
      disabled={providersStore.pending}
      onclick={remove}
    >
      <Trash2 size={16} strokeWidth={2.2} />
    </button>
  </div>

  {#if formError}
    <div class="notice notice-bad mt-2" role="alert"><span class="min-w-0 break-words">{formError}</span></div>
  {/if}

  <div>
    <Section title={t('llm.sec_connection')} hint={t('llm.sec_connection_hint')}>
      <div>
        <label class="label" for="provider-protocol">{t('llm.protocol')}</label>
        <Select id="provider-protocol" bind:value={protocol}>
          {#each protocols as option (option.id)}
            <option value={option.id}>{option.name}</option>
          {/each}
          {#if !protocols.some((option) => option.id === protocol)}
            <option value={protocol}>{protocol}</option>
          {/if}
        </Select>
      </div>
      <div>
        <label class="label" for="provider-url">{t('llm.base_url')}</label>
        <input
          id="provider-url"
          class="input mono"
          spellcheck="false"
          placeholder={protocolInfo?.default_base_url ?? 'https://'}
          bind:value={baseUrl}
        />
      </div>
      <div>
        <label class="label" for="provider-key">{t('llm.api_key')}</label>
        <SecretInput
          id="provider-key"
          bind:value={apiKey}
          placeholder={provider.api_key_configured ? t('llm.key_keep') : ''}
        />
        {#if provider.api_key_configured}
          <label class="mt-2.5 flex items-center gap-2.5 text-[14px] text-fg2">
            <input type="checkbox" class="check" bind:checked={clearKey} />
            {t('llm.key_clear')}
          </label>
        {:else}
          <p class="m-0 mt-2 hint">{t('llm.key_hint')}</p>
        {/if}
      </div>
    </Section>

    <Section title={t('llm.sec_test')} hint={t('llm.sec_test_hint')}>
      <div class="flex flex-wrap items-end gap-2.5">
        <div class="min-w-0 flex-1 basis-[220px]">
          <label class="label" for="provider-test-model">{t('llm.test_model')}</label>
          <input
            id="provider-test-model"
            class="input mono"
            spellcheck="false"
            list="provider-test-models"
            placeholder="gpt-4o-mini"
            bind:value={testModel}
          />
          <datalist id="provider-test-models">
            {#each upstreamModels as model (model)}
              <option value={model}></option>
            {/each}
          </datalist>
        </div>
        <button type="button" class="btn" disabled={testing} onclick={() => void test()}>
          <PlugZap size={16} strokeWidth={2.2} />
          {testing ? t('llm.testing') : t('llm.test')}
        </button>
      </div>
      {#if testResult && !testing}
        {#if testResult.status === 'ok'}
          <div class="notice notice-ok flex-col gap-1">
            <b class="font-extrabold">
              {t('llm.test_ok', { ms: testResult.latency_ms })}
            </b>
            {#if testResult.reply}
              <span class="line-clamp-3 min-w-0 break-words">{testResult.reply}</span>
            {/if}
          </div>
        {:else}
          <div class="notice notice-bad flex-col gap-1">
            <b class="font-extrabold">{t('llm.test_failed')}</b>
            <span class="min-w-0 break-words">{testResult.error}</span>
          </div>
        {/if}
      {/if}
    </Section>

    {#if showAdvanced}
      <Section title={t('llm.sec_defaults')} hint={t('llm.sec_defaults_hint')}>
        <div class="grid gap-3.5 sm:grid-cols-2">
          <div>
            <label class="label" for="provider-temperature">{t('llm.temperature')}</label>
            <input
              id="provider-temperature"
              class="input"
              inputmode="decimal"
              placeholder={t('llm.unset')}
              bind:value={temperature}
            />
          </div>
          <div>
            <label class="label" for="provider-max-tokens">{t('llm.max_tokens')}</label>
            <input
              id="provider-max-tokens"
              class="input"
              inputmode="numeric"
              placeholder={t('llm.unset')}
              bind:value={maxTokens}
            />
          </div>
        </div>
      </Section>
    {/if}
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
      <span class="text-[13.5px] text-fg2">{t('llm.advanced_summary')}</span>
    {/if}
  </div>

  {#if changeCount > 0}
    <div class="sticky bottom-4 z-10 mt-6 flex justify-center">
      <div
        class="flex max-w-full items-center gap-3 rounded-[26px] bg-bar py-2 pr-2 pl-5 text-[14.5px] font-bold text-on-bar shadow-[var(--k-pop)]"
      >
        <span class="truncate">
          {changeCount === 1
            ? t('instances.bar_changed_one')
            : t('instances.bar_changed', { n: changeCount })}
        </span>
        <button
          type="button"
          class="btn btn-quiet btn-sm text-on-bar! opacity-75 hover:opacity-100 hover:bg-transparent!"
          disabled={providersStore.pending}
          onclick={reset}
        >
          {t('instances.discard')}
        </button>
        <button
          type="button"
          class="btn btn-primary btn-sm"
          disabled={providersStore.pending}
          onclick={() => void save()}
        >
          {providersStore.pending ? t('instances.saving') : t('instances.save_changes')}
        </button>
      </div>
    </div>
  {/if}
</section>
