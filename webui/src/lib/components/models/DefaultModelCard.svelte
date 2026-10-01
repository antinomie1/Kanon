<script lang="ts">
import { t } from '../../stores/i18n.svelte';
import { CAPABILITY_FLAGS, modelsStore } from '../../stores/models.svelte';
import Select from '../ui/Select.svelte';
import { changeDefaultModel } from './defaultModel';
import { compactTokens, modelName } from './modelFormat';

/**
 * The node's one global default model: what answers whenever an instance picks no model itself.
 *
 * A change applies at once (the node validates, stores and hot-swaps it before answering) and the
 * toast offers an undo, the same as every other switch in the console.
 */

/** Picker value; kept in sync with the node so a refused change snaps back. */
let chosen = $state('');
$effect(() => {
  chosen = modelsStore.defaultModel ?? '';
});

/**
 * Catalog models grouped by provider. The current default is listed even when the catalog no
 * longer describes it, so the picker never shows a blank for a model the node is really using.
 */
const groups = $derived.by(() => {
  const byProvider = new Map<string, { reference: string; label: string }[]>();
  for (const spec of modelsStore.models) {
    const list = byProvider.get(spec.provider) ?? [];
    list.push({
      reference: modelsStore.referenceOf(spec),
      label: modelName(spec),
    });
    byProvider.set(spec.provider, list);
  }
  const current = modelsStore.defaultModel;
  if (current && !modelsStore.defaultSpec) {
    const slash = current.indexOf('/');
    const provider = current.slice(0, slash);
    const list = byProvider.get(provider) ?? [];
    list.push({ reference: current, label: current.slice(slash + 1) });
    byProvider.set(provider, list);
  }
  return [...byProvider.entries()];
});

const spec = $derived(modelsStore.defaultSpec);

/** Takes the picked value from the event, not `chosen`, so listener order cannot matter. */
async function apply(next: string) {
  // On refusal the node keeps its previous default: put the picker back on it.
  if (!(await changeDefaultModel(next || null)))
    chosen = modelsStore.defaultModel ?? '';
}
</script>

<section class="card flex flex-wrap items-start gap-x-8 gap-y-4 px-5 py-5 sm:px-7">
  <div class="min-w-0 flex-1 basis-[300px]">
    <h2 class="m-0 text-[17px] font-extrabold">{t('llm.default_title')}</h2>
    <p class="m-0 mt-1 max-w-[56ch] hint">{t('llm.default_hint')}</p>
    {#if spec}
      <div class="mt-3 flex flex-wrap items-center gap-1.5">
        {#each CAPABILITY_FLAGS as flag (flag)}
          {#if spec.capabilities[flag]}
            <span class="chip chip-sm chip-muted">{t(`models.cap_${flag}`)}</span>
          {/if}
        {/each}
        {#if spec.context_length}
          <span class="ml-1 text-[12.5px] text-fg3">
            {t('llm.context_short', { n: compactTokens(spec.context_length) })}
          </span>
        {/if}
      </div>
    {/if}
  </div>

  <div class="flex w-full flex-col gap-2 sm:w-[380px]">
    <label class="sr-only" for="default-model">{t('llm.default_title')}</label>
    <Select
      id="default-model"
      bind:value={chosen}
      disabled={modelsStore.saving || groups.length === 0}
      onchange={(e) => void apply(e.currentTarget.value)}
    >
      <option value="">{t('llm.default_unset')}</option>
      {#each groups as [provider, entries] (provider)}
        <optgroup label={provider}>
          {#each entries as entry (entry.reference)}
            <option value={entry.reference}>{entry.label}</option>
          {/each}
        </optgroup>
      {/each}
    </Select>
    {#if groups.length === 0}
      <p class="m-0 hint">{t('llm.default_no_models')}</p>
    {:else if !modelsStore.defaultModel}
      <div class="notice notice-warn">{t('llm.default_missing')}</div>
    {:else}
      <p class="m-0 truncate font-mono text-[12.5px] text-fg3" title={modelsStore.defaultModel}>
        {modelsStore.defaultModel}
      </p>
    {/if}
  </div>
</section>
