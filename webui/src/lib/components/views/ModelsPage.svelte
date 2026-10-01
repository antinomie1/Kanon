<script lang="ts">
import { Plus, Server } from 'lucide-svelte';
import { untrack } from 'svelte';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { modelsStore } from '../../stores/models.svelte';
import { providersStore } from '../../stores/providers.svelte';
import { router } from '../../stores/router.svelte';
import AddProviderModal from '../models/AddProviderModal.svelte';
import DefaultModelCard from '../models/DefaultModelCard.svelte';
import ProviderEditor from '../models/ProviderEditor.svelte';
import ProviderModels from '../models/ProviderModels.svelte';
import EmptyState from '../ui/EmptyState.svelte';
import PageHead from '../ui/PageHead.svelte';

/**
 * Models page: the node's one default model on top, then the provider endpoints that serve models.
 *
 * The route parameter names the provider open in the editor (`#/models/<provider>`), so a
 * provider can be linked to and the back button walks between them.
 */

const providers = $derived(providersStore.providers);
const loaded = $derived(providersStore.catalog !== null);

/** Provider shown in the editor; follows the route. */
let openedName = $state<string | null>(null);
/** Whether the editor holds edits that have not been saved; reported by the editor itself. */
let dirty = $state(false);
let addOpen = $state(false);

const opened = $derived(
  providers.find((provider) => provider.name === openedName),
);

/**
 * Follows the route. The bare page opens the first provider; leaving a provider with unsaved edits
 * asks first, and "keep editing" puts the address back.
 */
async function follow(param: string | null) {
  if (param === null) {
    const first = providersStore.providers[0];
    if (first) router.replaceParam(first.name);
    else openedName = null;
    return;
  }
  if (param === openedName) return;
  if (openedName !== null && dirty) {
    const leave = await confirmDialog({
      title: t('instances.discard_title'),
      message: t('llm.discard_text'),
      confirm: t('instances.discard_confirm'),
      cancel: t('instances.keep_editing'),
    });
    if (!leave) {
      router.replaceParam(openedName);
      return;
    }
  }
  dirty = false;
  openedName = param;
}

$effect(() => {
  const param = router.param;
  // Re-run when the directory changes too, so a deleted provider hands over to the next one.
  void providersStore.catalog;
  if (!loaded) return;
  untrack(() => void follow(param));
});

function modelCount(provider: string): number {
  return modelsStore.models.filter((spec) => spec.provider === provider).length;
}

function servesDefault(provider: string): boolean {
  return modelsStore.defaultModel?.startsWith(`${provider}/`) ?? false;
}

function onAdded(name: string) {
  addOpen = false;
  dirty = false;
  router.navigate('models', name);
}

function onDeleted() {
  dirty = false;
  openedName = null;
  router.replaceParam(providersStore.providers[0]?.name ?? null);
}
</script>

<PageHead title={t('nav.models')}>
  {#snippet sub()}
    {#if loaded}
      <span>
        {providers.length === 1
          ? t('llm.summary_one', { models: modelsStore.models.length })
          : t('llm.summary', { providers: providers.length, models: modelsStore.models.length })}
      </span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <button type="button" class="btn btn-primary" onclick={() => (addOpen = true)}>
      <Plus size={16} strokeWidth={2.6} />
      {t('llm.add_provider')}
    </button>
  {/snippet}
</PageHead>

{#if !loaded}
  {#if providersStore.error}
    <div class="notice notice-bad">{providersStore.error}</div>
  {:else}
    <p class="m-0 px-1 hint">{t('common.loading')}</p>
  {/if}
{:else if providers.length === 0}
  <div class="card">
    <EmptyState icon={Server} title={t('llm.empty_title')} text={t('llm.empty_text')}>
      {#snippet action()}
        <button type="button" class="btn btn-primary" onclick={() => (addOpen = true)}>
          <Plus size={16} strokeWidth={2.6} />
          {t('llm.add_provider')}
        </button>
      {/snippet}
    </EmptyState>
  </div>
{:else}
  <DefaultModelCard />

  <div class="grid items-start gap-4 lg:grid-cols-[264px_minmax(0,1fr)]">
    <nav class="card flex flex-col gap-0.5 p-2" aria-label={t('llm.providers')}>
      {#each providers as provider (provider.name)}
        {@const on = provider.name === openedName}
        {@const count = modelCount(provider.name)}
        <a
          href="#/models/{encodeURIComponent(provider.name)}"
          aria-current={on ? 'page' : undefined}
          onclick={(e) => {
            e.preventDefault();
            if (!on) router.navigate('models', provider.name);
          }}
          class="flex flex-col rounded-xl px-3 py-2.5 leading-[1.35] text-fg no-underline {on
            ? 'bg-accent-tint'
            : 'hover:bg-sunk'}"
        >
          <span class="flex min-w-0 items-center gap-2">
            <span class="truncate text-[15px] font-extrabold {on ? 'text-accent-fg' : ''}">{provider.name}</span>
            {#if servesDefault(provider.name)}
              <span class="chip chip-sm chip-accent">{t('llm.default_chip')}</span>
            {/if}
          </span>
          <span class="flex items-center gap-1.5 text-[12.5px] whitespace-nowrap text-fg2">
            {#if !provider.api_key_configured}
              <i class="dot dot-warn"></i>{t('llm.no_key')}
            {:else if count === 1}
              {t('llm.model_count_one')}
            {:else}
              {t('llm.model_count', { n: count })}
            {/if}
          </span>
        </a>
      {/each}
    </nav>

    {#if opened}
      <div class="flex min-w-0 flex-col gap-4">
        {#key opened.name}
          <ProviderEditor provider={opened} bind:dirty {onDeleted} />
          <ProviderModels provider={opened.name} />
        {/key}
      </div>
    {:else if openedName !== null}
      <div class="card">
        <EmptyState
          compact
          title={t('llm.missing_title')}
          text={t('llm.missing_text', { name: openedName })}
        />
      </div>
    {/if}
  </div>
{/if}

<AddProviderModal open={addOpen} onclose={() => (addOpen = false)} onadded={onAdded} />
