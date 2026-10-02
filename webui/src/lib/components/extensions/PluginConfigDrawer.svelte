<script lang="ts">
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { formSupported, type JsonSchema } from '../../configSchema';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { PluginConfigResponse } from '../../types';
import Button from '../ui/Button.svelte';
import Modal from '../ui/Modal.svelte';
import Seg from '../ui/Seg.svelte';
import ConfigForm from './ConfigForm.svelte';

/**
 * Settings of one plugin, as a form built from the schema its manifest declares, or as JSON.
 *
 * The form covers the common field types (see `configSchema.ts`); a plugin whose schema has no
 * fields opens straight in JSON. Both views edit the same document, and the node validates it
 * against the schema on save. Saves carry the version the node reported, so two consoles editing at
 * once cannot silently overwrite each other.
 */
let {
  pluginId,
  name,
  texts = {},
  onclose,
}: {
  /** Plugin whose settings are shown; `null` keeps the drawer closed. */
  pluginId: string | null;
  /** Display name for the title. */
  name: string;
  /** The plugin's translations for the console's language (field titles and hints). */
  texts?: Record<string, string>;
  onclose: () => void;
} = $props();

let current = $state<PluginConfigResponse | null>(null);
/** The settings being edited; the form's copy, and the JSON view's whenever it parses. */
let values = $state<Record<string, unknown>>({});
let raw = $state('');
let mode = $state<'form' | 'json'>('form');
const hasForm = $derived(!!current && formSupported(current.schema));
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
    values = res.values;
    raw = JSON.stringify(res.values, null, 2);
    mode = formSupported(res.schema) ? 'form' : 'json';
  } catch (e) {
    loadError = errorText(e);
  }
}

// Reload whenever the drawer is pointed at a different plugin.
$effect(() => {
  const id = pluginId;
  if (id) untrack(() => void load(id));
});

/** Parses the JSON view; `null` (with the reason shown) when it is not an object. */
function parseRaw(): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      saveError = t('extensions.config_not_object');
      return null;
    }
    return parsed as Record<string, unknown>;
  } catch (e) {
    saveError = t('extensions.config_bad_json', { error: errorText(e) });
    return null;
  }
}

/** Switches views, carrying the edits across; JSON that does not parse keeps its view open. */
function switchMode(next: 'form' | 'json') {
  saveError = null;
  if (next === 'json') {
    raw = JSON.stringify(values, null, 2);
  } else {
    const parsed = parseRaw();
    if (!parsed) return;
    values = parsed;
  }
  mode = next;
}

async function save() {
  if (!pluginId || !current) return;
  saveError = null;
  const parsed = mode === 'form' ? values : parseRaw();
  if (!parsed) return;
  saving = true;
  try {
    await api.updatePluginConfig(pluginId, parsed, current.version);
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
    {#if hasForm}
      <div class="mt-4">
        <Seg
          label={t('extensions.config_view')}
          size="sm"
          value={mode}
          onchange={switchMode}
          options={[
            { value: 'form', label: t('extensions.config_form') },
            { value: 'json', label: 'JSON' },
          ]}
        />
      </div>
    {/if}
    {#if mode === 'form' && hasForm}
      <div class="mt-5">
        <ConfigForm
          schema={current.schema as JsonSchema}
          value={values}
          onchange={(next) => (values = next)}
          {texts}
        />
      </div>
    {:else}
      <label class="label mt-4" for="plugin-config">{t('extensions.config_json')}</label>
      <textarea
        id="plugin-config"
        class="input mono min-h-[320px]"
        spellcheck="false"
        bind:value={raw}
      ></textarea>
    {/if}
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
    <Button type="button" disabled={saving} onclick={onclose}>{t('common.cancel')}</Button>
    <Button type="button" variant="filled" disabled={!current || saving} onclick={() => void save()}>
      {saving ? t('platforms.saving') : t('extensions.config_save')}
    </Button>
  {/snippet}
</Modal>
