<script lang="ts">
import { Check } from 'lucide-svelte';
import { untrack } from 'svelte';
import { t } from '../../stores/i18n.svelte';
import {
  CAPABILITY_FLAGS,
  DEFAULT_CAPABILITIES,
  modelsStore,
} from '../../stores/models.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { ModelCapabilities, ModelSpec } from '../../types';
import Modal from '../ui/Modal.svelte';
import { optionalNumber } from './modelFormat';

/**
 * Adds a model to one provider, or corrects what the catalog says about an existing one.
 *
 * The model id is fixed while editing: it is half of the `<provider>/<model>` reference instances
 * point at, so renaming it would really create a second entry.
 */
let {
  provider,
  spec,
  open,
  onclose,
}: {
  provider: string;
  /** Entry to edit; `null` adds a new one. */
  spec: ModelSpec | null;
  open: boolean;
  onclose: () => void;
} = $props();

let model = $state('');
let displayName = $state('');
let contextLength = $state('');
let maxOutput = $state('');
let temperature = $state('');
let capabilities = $state<ModelCapabilities>({ ...DEFAULT_CAPABILITIES });
let error = $state<string | null>(null);

// Fill the form each time the dialog opens; only `open` is tracked, so a catalog refresh while
// it is open cannot wipe what is being typed.
$effect(() => {
  if (!open) return;
  untrack(() => {
    model = spec?.model ?? '';
    displayName = spec?.display_name ?? '';
    contextLength = spec?.context_length?.toString() ?? '';
    maxOutput = spec?.max_output_tokens?.toString() ?? '';
    temperature = spec?.temperature?.toString() ?? '';
    capabilities = { ...(spec?.capabilities ?? DEFAULT_CAPABILITIES) };
    error = null;
  });
});

async function save() {
  error = null;
  const id = model.trim();
  if (!id) return;
  const context = optionalNumber(contextLength);
  const output = optionalNumber(maxOutput);
  const temp = optionalNumber(temperature);
  if (context === null || output === null || temp === null) {
    error = t('llm.bad_number');
    return;
  }
  // Adding an id the catalog already has would silently replace that entry.
  if (
    !spec &&
    modelsStore.models.some((m) => m.provider === provider && m.model === id)
  ) {
    error = t('llm.model_exists', { model: id });
    return;
  }
  const next: ModelSpec = {
    provider,
    model: id,
    capabilities: { ...capabilities },
    // The node sets this itself; sending it keeps the payload self-describing.
    source: 'manual',
  };
  if (displayName.trim()) next.display_name = displayName.trim();
  if (context !== undefined) next.context_length = context;
  if (output !== undefined) next.max_output_tokens = output;
  if (temp !== undefined) next.temperature = temp;

  if (await modelsStore.upsert(next)) {
    toasts.ok(
      t(spec ? 'llm.model_saved_toast' : 'llm.model_added_toast', {
        name: displayName.trim() || id,
      }),
    );
    onclose();
  } else {
    error = modelsStore.error;
  }
}
</script>

<Modal
  {open}
  title={spec ? t('llm.model_edit_title', { name: spec.display_name || spec.model }) : t('llm.model_add_title', { provider })}
  locked={modelsStore.saving}
  {onclose}
>
  <form
    id="model-editor"
    class="flex flex-col gap-4"
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <div class="grid gap-4 sm:grid-cols-2">
      <div>
        <label class="label" for="model-id">{t('llm.model_id')}</label>
        <input
          id="model-id"
          class="input mono"
          spellcheck="false"
          placeholder="gpt-4o"
          disabled={spec !== null}
          bind:value={model}
        />
      </div>
      <div>
        <label class="label" for="model-name">{t('llm.model_name')}</label>
        <input id="model-name" class="input" placeholder={t('llm.optional')} bind:value={displayName} />
      </div>
    </div>
    <p class="m-0 -mt-1 hint">{t('llm.model_id_hint', { provider, model: model.trim() || 'gpt-4o' })}</p>

    <fieldset class="m-0 border-0 p-0">
      <legend class="label">{t('llm.capabilities')}</legend>
      <div class="flex flex-wrap gap-2">
        {#each CAPABILITY_FLAGS as flag (flag)}
          {@const on = capabilities[flag]}
          <button
            type="button"
            aria-pressed={on}
            onclick={() => (capabilities = { ...capabilities, [flag]: !on })}
            class="inline-flex h-[34px] items-center gap-1.5 rounded-full px-3.5 text-[13.5px] font-bold whitespace-nowrap transition-colors {on
              ? 'bg-accent-tint text-accent-fg'
              : 'bg-sunk text-fg2 hover:text-fg'}"
          >
            {#if on}<Check size={14} strokeWidth={2.8} />{/if}
            {t(`models.cap_${flag}`)}
          </button>
        {/each}
      </div>
      <p class="m-0 mt-2 hint">{t('llm.capabilities_hint')}</p>
    </fieldset>

    <div class="grid gap-4 sm:grid-cols-3">
      <div>
        <label class="label" for="model-context">{t('llm.context_length')}</label>
        <input id="model-context" class="input" inputmode="numeric" placeholder="128000" bind:value={contextLength} />
      </div>
      <div>
        <label class="label" for="model-output">{t('llm.max_output')}</label>
        <input id="model-output" class="input" inputmode="numeric" placeholder={t('llm.unset')} bind:value={maxOutput} />
      </div>
      <div>
        <label class="label" for="model-temperature">{t('llm.temperature')}</label>
        <input id="model-temperature" class="input" inputmode="decimal" placeholder={t('llm.unset')} bind:value={temperature} />
      </div>
    </div>

    {#if spec?.source === 'upstream'}
      <p class="m-0 hint">{t('llm.model_manual_hint')}</p>
    {/if}
    {#if error}
      <div class="notice notice-bad" role="alert"><span class="min-w-0 break-words">{error}</span></div>
    {/if}
  </form>

  {#snippet footer()}
    <button type="button" class="btn" disabled={modelsStore.saving} onclick={onclose}>
      {t('common.cancel')}
    </button>
    <button type="submit" form="model-editor" class="btn btn-primary" disabled={!model.trim() || modelsStore.saving}>
      {modelsStore.saving ? t('instances.saving') : spec ? t('instances.save_changes') : t('llm.model_add')}
    </button>
  {/snippet}
</Modal>
