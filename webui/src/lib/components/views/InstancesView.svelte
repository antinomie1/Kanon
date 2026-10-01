<script lang="ts">
import { Boxes, Plus } from 'lucide-svelte';
import { untrack } from 'svelte';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { router } from '../../stores/router.svelte';
import InstanceEditor from '../instances/InstanceEditor.svelte';
import EmptyState from '../ui/EmptyState.svelte';
import PageHead from '../ui/PageHead.svelte';

const instances = $derived(instancesStore.instances);
const loaded = $derived(instancesStore.catalog !== null);

/** Route parameter the editor currently shows (an instance id or `new`). */
let openedFor = $state<string | null>(null);
/** Set when the route names an instance the catalog does not have. */
let missing = $state<string | null>(null);

/**
 * Follows the route: `#/instances/<id>` edits that instance, `#/instances/new` creates one, and
 * the bare page selects the first instance. Leaving a draft with unsaved changes asks first, and
 * answering "keep editing" puts the address back.
 */
async function follow(param: string | null) {
  if (param !== null && param === openedFor && instancesStore.isFormOpen)
    return;

  if (param === null) {
    const first = instancesStore.instances[0];
    if (first) router.replaceParam(first.id);
    return;
  }

  if (openedFor !== null && instancesStore.changeCount > 0) {
    const leave = await confirmDialog({
      title: t('instances.discard_title'),
      message: t('instances.discard_text'),
      confirm: t('instances.discard_confirm'),
      cancel: t('instances.keep_editing'),
    });
    if (!leave) {
      router.replaceParam(openedFor);
      return;
    }
  }

  missing = null;
  if (param === 'new') {
    instancesStore.openCreate();
    openedFor = 'new';
    return;
  }
  const instance = instancesStore.find(param);
  if (instance) {
    instancesStore.openEdit(instance);
    openedFor = param;
  } else {
    instancesStore.closeForm();
    openedFor = null;
    missing = param;
  }
}

$effect(() => {
  const param = router.param;
  if (!loaded) return;
  untrack(() => void follow(param));
});

function select(id: string) {
  if (id === openedFor) return;
  router.navigate('instances', id);
}

/** Called by the editor after a save or delete moved the selection. */
function onSaved(id: string) {
  openedFor = id;
  router.replaceParam(id);
}

function onDeleted() {
  openedFor = null;
  router.navigate('instances');
}
</script>

<PageHead title={t('nav.instances')}>
  {#snippet sub()}
    {#if loaded}
      <span>
        {t('instances.summary', {
          total: instances.length,
          on: instancesStore.enabledCount,
        })}
      </span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <button type="button" class="btn btn-primary" onclick={() => router.navigate('instances', 'new')}>
      <Plus size={16} strokeWidth={2.2} />
      {t('instances.new')}
    </button>
  {/snippet}
</PageHead>

{#if !loaded}
  {#if instancesStore.error}
    <div class="notice notice-bad">{instancesStore.error}</div>
  {:else}
    <p class="m-0 px-1 hint">{t('common.loading')}</p>
  {/if}
{:else if instances.length === 0 && openedFor !== 'new'}
  <div class="card">
    <EmptyState icon={Boxes} title={t('home.empty_title')} text={t('home.empty_text')}>
      {#snippet action()}
        <button type="button" class="btn btn-primary" onclick={() => router.navigate('instances', 'new')}>
          <Plus size={16} strokeWidth={2.2} />
          {t('instances.new')}
        </button>
      {/snippet}
    </EmptyState>
  </div>
{:else}
  <div class="grid items-start gap-4 lg:grid-cols-[264px_minmax(0,1fr)]">
    <nav class="card flex flex-col gap-0.5 p-2" aria-label={t('nav.instances')}>
      {#each instances as instance (instance.id)}
        {@const on = instance.id === openedFor}
        {@const trouble =
          instance.enabled && instance.adapter_status.some((s) => !(s.known && s.connected))}
        <a
          href="#/instances/{encodeURIComponent(instance.id)}"
          aria-current={on ? 'page' : undefined}
          onclick={(e) => {
            e.preventDefault();
            select(instance.id);
          }}
          class="flex flex-col rounded-xl px-3 py-2.5 leading-[1.35] text-fg no-underline {on
            ? 'bg-accent-tint'
            : 'hover:bg-sunk'}"
        >
          <span class="truncate text-[15px] font-semibold {on ? 'text-accent-fg' : ''}">{instance.name}</span>
          <span class="flex items-center gap-1.5 text-[12.5px] whitespace-nowrap text-fg2">
            {#if !instance.enabled}
              {t('instances.state_off')}
            {:else if trouble}
              <i class="dot dot-warn"></i>{t('instances.state_trouble')}
            {:else}
              <i class="dot dot-ok"></i>{t('instances.state_on')}
            {/if}
          </span>
        </a>
      {/each}
      {#if openedFor === 'new'}
        <div class="flex flex-col rounded-xl bg-accent-tint px-3 py-2.5 leading-[1.35]">
          <span class="truncate text-[15px] font-semibold text-accent-fg">
            {instancesStore.formName.trim() || t('instances.new_unnamed')}
          </span>
          <span class="text-[12.5px] text-fg2">{t('instances.state_draft')}</span>
        </div>
      {/if}
    </nav>

    {#if instancesStore.isFormOpen}
      <InstanceEditor {onSaved} {onDeleted} />
    {:else if missing}
      <div class="card">
        <EmptyState
          compact
          title={t('instances.missing_title')}
          text={t('instances.missing_text', { id: missing })}
        />
      </div>
    {/if}
  </div>
{/if}
