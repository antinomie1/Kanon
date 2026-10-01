<script lang="ts">
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { PluginConfigResponse } from '../../types';
import Modal from '../ui/Modal.svelte';

/**
 * Settings of one plugin, edited as JSON.
 *
 * Plugin settings are whatever the plugin's manifest schema describes, so there is no fixed form;
 * the node validates the document against that schema on save. Saves carry the version the node
 * reported, so two consoles editing at once cannot silently overwrite each other.
 */
let {
  pluginId,
  name,
  onclose,
}: {
  /** Plugin whose settings are shown; `null` keeps the drawer closed. */
  pluginId: string | null;
  /** Display name for the title. */
  name: string;
  onclose: () => void;
} = $props();

let current = $state<PluginConfigResponse | null>(null);
let raw = $state('');
let loadError = $state<string | null>(null);
let saveError = $state<string | null>(null);
let saving = $state(false);

async function load(id: string) {
  current = null;
  loadError = null;
  saveError = null;
  try {
    const res = await api.getPluginConfig(id);
    current = res;
    raw = JSON.stringify(res.values, null, 2);
  } catch (e) {
    loadError = errorText(e);
  }
}

// Reload whenever the drawer is pointed at a different plugin.
$effect(() => {
  const id = pluginId;
  if (id) untrack(() => void load(id));
});

async function save() {
  if (!pluginId || !current) return;
  saveError = null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (e) {
    saveError = t('extensions.config_bad_json', { error: errorText(e) });
    return;
  }
  saving = true;
  try {
    await api.updatePluginConfig(
      pluginId,
      parsed as Record<string, unknown>,
      current.version,
    );
    toasts.ok(t('extensions.config_saved', { name }));
    onclose();
  } catch (e) {
    saveError = errorText(e);
  } finally {
    saving = false;
  }
}
</script>

<Modal
  open={pluginId !== null}
  title={t('extensions.config_title', { name })}
  variant="drawer"
  width="max-w-[620px]"
  locked={saving}
  {onclose}
>
  {#if loadError}
    <div class="notice notice-bad"><span class="min-w-0 break-words">{loadError}</span></div>
  {:else if !current}
    <p class="m-0 hint">{t('common.loading')}</p>
  {:else}
    <p class="m-0 hint">
      {current.persisted ? t('extensions.config_hint') : t('extensions.config_defaults')}
    </p>
    <label class="label mt-4" for="plugin-config">{t('extensions.config_json')}</label>
    <textarea
      id="plugin-config"
      class="input mono min-h-[320px]"
      spellcheck="false"
      bind:value={raw}
    ></textarea>
    {#if saveError}
      <div class="notice notice-bad mt-3"><span class="min-w-0 break-words">{saveError}</span></div>
    {/if}
    <details class="mt-5">
      <summary class="cursor-pointer text-[14px] font-medium text-fg2">{t('extensions.config_schema')}</summary>
      <pre class="scroll-thin m-0 mt-2 overflow-x-auto rounded-xl bg-sunk p-3.5 text-[12.5px]">{JSON.stringify(current.schema, null, 2)}</pre>
    </details>
    <p class="m-0 mt-4 text-[12.5px] text-fg3">{t('extensions.config_version', { n: current.version })}</p>
  {/if}

  {#snippet footer()}
    <button type="button" class="btn" disabled={saving} onclick={onclose}>{t('common.cancel')}</button>
    <button type="button" class="btn btn-primary" disabled={!current || saving} onclick={() => void save()}>
      {saving ? t('platforms.saving') : t('extensions.config_save')}
    </button>
  {/snippet}
</Modal>
