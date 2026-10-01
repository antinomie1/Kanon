<script lang="ts">
import { Pencil, Plus, RefreshCw, Trash2 } from 'lucide-svelte';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { ModelCapabilities, ModelSpec } from '../../types';
import { changeDefaultModel } from './defaultModel';
import ModelEditorModal from './ModelEditorModal.svelte';
import { compactTokens, modelName } from './modelFormat';

/**
 * Models one provider serves. Unlike the endpoint above it, each action here applies at once:
 * setting the default, removing a model and fetching the list are all single, reversible steps.
 */
let { provider }: { provider: string } = $props();

const models = $derived(
  modelsStore.models.filter((spec) => spec.provider === provider),
);

/** Capabilities worth pointing out; plain text is what every model does. */
const NOTABLE: (keyof ModelCapabilities)[] = [
  'vision',
  'audio',
  'video',
  'tool_calling',
  'reasoning',
];

let discovering = $state(false);
/** Model open in the editor: a spec to edit, `'new'` to add one, `null` when closed. */
let editing = $state<ModelSpec | 'new' | null>(null);

async function discover() {
  discovering = true;
  const res = await modelsStore.discover(provider, true);
  discovering = false;
  if (res) {
    toasts.ok(
      t('llm.discover_toast', {
        found: res.discovered.length,
        saved: res.persisted,
      }),
    );
  } else {
    toasts.error(t('llm.discover_failed', { error: modelsStore.error ?? '' }));
  }
}

async function remove(spec: ModelSpec) {
  const reference = modelsStore.referenceOf(spec);
  const isDefault = reference === modelsStore.defaultModel;
  const yes = await confirmDialog({
    title: t('llm.model_remove_title', { name: modelName(spec) }),
    message: isDefault
      ? t('llm.model_remove_default')
      : t('llm.model_remove_text'),
    confirm: t('llm.remove'),
    danger: true,
  });
  if (!yes) return;
  if (await modelsStore.remove(reference)) {
    toasts.ok(t('llm.model_removed_toast', { name: modelName(spec) }));
  } else {
    toasts.error(modelsStore.error ?? t('common.error'));
  }
}
</script>

<section class="card px-5 pt-5 pb-3 sm:px-7">
  <div class="flex flex-wrap items-start justify-between gap-x-4 gap-y-3">
    <div class="min-w-0 flex-1 basis-[260px]">
      <h2 class="m-0 text-[17px] font-extrabold">
        {t('llm.models_title')}
        <span class="ml-1 font-bold text-fg3">{models.length}</span>
      </h2>
      <p class="m-0 mt-1 max-w-[60ch] hint">{t('llm.models_hint', { provider })}</p>
    </div>
    <div class="flex flex-wrap gap-2.5">
      <button type="button" class="btn btn-sm" disabled={discovering} onclick={() => void discover()}>
        <RefreshCw size={15} strokeWidth={2.4} class={discovering ? 'animate-spin' : ''} />
        {discovering ? t('llm.discovering') : t('llm.discover')}
      </button>
      <button type="button" class="btn btn-sm" onclick={() => (editing = 'new')}>
        <Plus size={15} strokeWidth={2.6} />
        {t('llm.model_add')}
      </button>
    </div>
  </div>

  {#if models.length === 0}
    <p class="m-0 mt-4 mb-2 rounded-[14px] bg-sunk px-4 py-3.5 text-[14px] text-fg2">
      {t('llm.models_empty')}
    </p>
  {:else}
    <ul class="m-0 mt-3 list-none p-0">
      {#each models as spec (spec.model)}
        {@const reference = modelsStore.referenceOf(spec)}
        {@const isDefault = reference === modelsStore.defaultModel}
        {@const name = modelName(spec)}
        <li class="flex flex-wrap items-center gap-x-5 gap-y-2 border-t border-line py-3.5 first:border-t-0">
          <div class="min-w-0 flex-1 basis-[280px]">
            <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
              <span class="min-w-0 truncate text-[15px] font-extrabold">{name}</span>
              {#if isDefault}
                <span class="chip chip-sm chip-accent">{t('llm.default_chip')}</span>
              {/if}
              {#each NOTABLE as flag (flag)}
                {#if spec.capabilities[flag]}
                  <span class="chip chip-sm chip-muted">{t(`models.cap_${flag}`)}</span>
                {/if}
              {/each}
            </div>
            <p class="m-0 mt-0.5 flex flex-wrap gap-x-4 text-[12.5px] text-fg3">
              {#if name !== spec.model}<span class="font-mono">{spec.model}</span>{/if}
              {#if spec.context_length}
                <span>{t('llm.context_short', { n: compactTokens(spec.context_length) })}</span>
              {/if}
              {#if spec.source === 'manual'}<span>{t('llm.source_manual')}</span>{/if}
            </p>
          </div>
          <div class="ml-auto flex items-center gap-1.5">
            {#if !isDefault}
              <button
                type="button"
                class="btn btn-sm btn-quiet"
                disabled={modelsStore.saving}
                onclick={() => void changeDefaultModel(reference)}
              >
                {t('llm.make_default')}
              </button>
            {/if}
            <button
              type="button"
              class="btn btn-sm btn-quiet btn-icon"
              title={t('llm.model_edit')}
              aria-label={t('llm.model_edit_title', { name })}
              onclick={() => (editing = spec)}
            >
              <Pencil size={15} strokeWidth={2.2} />
            </button>
            <button
              type="button"
              class="btn btn-sm btn-quiet btn-icon"
              title={t('llm.remove')}
              aria-label={t('llm.model_remove_title', { name })}
              disabled={modelsStore.saving}
              onclick={() => void remove(spec)}
            >
              <Trash2 size={15} strokeWidth={2.2} />
            </button>
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<ModelEditorModal
  {provider}
  spec={editing === 'new' ? null : editing}
  open={editing !== null}
  onclose={() => (editing = null)}
/>
