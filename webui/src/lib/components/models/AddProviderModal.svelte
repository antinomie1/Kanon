<script lang="ts">
import { untrack } from 'svelte';
import { t } from '../../stores/i18n.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { providersStore } from '../../stores/providers.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { ProviderPreset, UpsertProviderRequest } from '../../types';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';
import SecretInput from '../ui/SecretInput.svelte';
import Select from '../ui/Select.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Connects a new model provider. Picking one of the well-known services fills in its format and
 * address, so most people only choose a service and paste a key.
 *
 * On save the node also reads the provider's model list, which is why the toast can say how many
 * models arrived with it.
 */
let {
  open,
  onclose,
  onadded,
}: {
  open: boolean;
  onclose: () => void;
  /** Called with the new provider's name once the node has stored it. */
  onadded: (name: string) => void;
} = $props();

let name = $state('');
let protocol = $state('openai');
let baseUrl = $state('');
let apiKey = $state('');
let preset = $state<string | null>(null);
let error = $state<string | null>(null);

const protocols = $derived(providersStore.catalog?.available_protocols ?? []);
const presets = $derived(providersStore.catalog?.presets ?? []);
const protocolInfo = $derived(protocols.find((p) => p.id === protocol));

$effect(() => {
  if (!open) return;
  untrack(() => {
    name = '';
    protocol = protocols.some((p) => p.id === 'openai')
      ? 'openai'
      : (protocols[0]?.id ?? 'openai');
    baseUrl = '';
    apiKey = '';
    preset = null;
    error = null;
  });
});

/** A preset only fills the form; nothing is stored until the form is submitted. */
function usePreset(choice: ProviderPreset) {
  preset = choice.id;
  name = choice.id;
  protocol = choice.protocol;
  baseUrl = choice.base_url;
  error = null;
}

async function add() {
  error = null;
  const trimmed = name.trim();
  if (!trimmed) return;
  // The name is the prefix of every model reference (`name/model`), so it cannot hold a slash,
  // and reusing a name would silently replace that provider.
  if (trimmed.includes('/')) {
    error = t('llm.name_slash');
    return;
  }
  if (providersStore.providers.some((p) => p.name === trimmed)) {
    error = t('llm.name_taken', { name: trimmed });
    return;
  }
  const url = baseUrl.trim() || protocolInfo?.default_base_url || '';
  if (!url) {
    error = t('llm.url_required');
    return;
  }
  const req: UpsertProviderRequest = { name: trimmed, protocol, base_url: url };
  if (apiKey.trim()) req.api_key = apiKey.trim();

  if (!(await providersStore.upsertProvider(req))) {
    error = providersStore.actionError;
    return;
  }
  const count = modelsStore.models.filter(
    (spec) => spec.provider === trimmed,
  ).length;
  toasts.ok(
    count > 0
      ? t('llm.added_toast_models', { name: trimmed, n: count })
      : t('llm.added_toast', { name: trimmed }),
  );
  onadded(trimmed);
}
</script>

<Modal {open} title={t('llm.add_provider')} locked={providersStore.pending} {onclose}>
  <form
    id="add-provider"
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void add();
    }}
  >
    {#if presets.length > 0}
      <div>
        <span class="label">{t('llm.presets')}</span>
        <div class="flex flex-wrap gap-2">
          {#each presets as choice (choice.id)}
            {@const on = preset === choice.id}
            <Button
              type="button"
              variant={on ? 'tonal' : 'outlined'}
              size="sm"
              aria-pressed={on}
              onclick={() => usePreset(choice)}
            >
              {choice.name}
            </Button>
          {/each}
        </div>
        <p class="m-0 mt-2 hint">{t('llm.presets_hint')}</p>
      </div>
    {/if}

    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="label" for="new-provider-name">{t('llm.name')}</label>
        <TextField
          id="new-provider-name"
          mono
          spellcheck="false"
          autocomplete="off"
          placeholder="openai"
          bind:value={name}
        />
      </div>
      <div>
        <label class="label" for="new-provider-protocol">{t('llm.protocol')}</label>
        <Select id="new-provider-protocol" bind:value={protocol}>
          {#each protocols as option (option.id)}
            <option value={option.id}>{option.name}</option>
          {/each}
        </Select>
      </div>
    </div>
    <p class="m-0 -mt-1 hint">{t('llm.name_hint', { name: name.trim() || 'openai' })}</p>

    <div>
      <label class="label" for="new-provider-url">{t('llm.base_url')}</label>
      <TextField
        id="new-provider-url"
        mono
        spellcheck="false"
        placeholder={protocolInfo?.default_base_url ?? 'https://'}
        bind:value={baseUrl}
      />
    </div>
    <div>
      <label class="label" for="new-provider-key">{t('llm.api_key')}</label>
      <SecretInput id="new-provider-key" bind:value={apiKey} />
      <p class="m-0 mt-2 hint">{t('llm.key_hint')}</p>
    </div>

    {#if error}
      <div class="notice notice-bad" role="alert"><span class="min-w-0 break-words">{error}</span></div>
    {/if}
  </form>

  {#snippet footer()}
    <Button type="button" disabled={providersStore.pending} onclick={onclose}>
      {t('common.cancel')}
    </Button>
    <Button
      type="submit"
      form="add-provider"
      variant="filled"
      disabled={!name.trim() || providersStore.pending}
    >
      {providersStore.pending ? t('llm.adding') : t('llm.add_provider')}
    </Button>
  {/snippet}
</Modal>
