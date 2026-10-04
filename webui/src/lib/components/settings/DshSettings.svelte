<script lang="ts">
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { DshConnection, DshSettings } from '../../types';
import Section from '../ui/Section.svelte';
import TextField from '../ui/TextField.svelte';
import Button from '../ui/Button.svelte';
import Select from '../ui/Select.svelte';

/** Connection coordinates live in Kanon; native settings retain DSH's revision contract. */
let connection = $state<DshConnection | null>(null);
let url = $state('http://127.0.0.1:3080');
let cookie = $state('');
let busy = $state(false);
let error = $state<string | null>(null);
let native = $state<DshSettings | null>(null);
let namespace = $state('');
let patch = $state('{}');
const section = $derived(native?.namespaces.find((entry) => entry.ns === namespace));

$effect(() => {
  let active = true;
  api.getDshConnection().then((value) => {
    if (!active) return;
    connection = value;
    if (value) { url = value.base_url; cookie = value.cookie_file ?? ''; }
  }).catch((cause) => { if (active) error = errorText(cause); });
  return () => { active = false; };
});

/** Save only transport coordinates; changing an endpoint invalidates its native settings view. */
async function save() {
  busy = true; error = null;
  try {
    connection = await api.setDshConnection({
      base_url: url.trim(), cookie_file: cookie.trim() || null,
      request_timeout_seconds: connection?.request_timeout_seconds ?? 30,
      turn_timeout_seconds: connection?.turn_timeout_seconds ?? 600,
    });
    native = null;
    toasts.ok(t('settings.saved_toast'));
  } catch (cause) { error = errorText(cause); }
  finally { busy = false; }
}

async function loadNative() {
  busy = true; error = null;
  try {
    native = await api.getDshSettings();
    namespace = native.namespaces[0]?.ns ?? '';
    patch = '{}';
  } catch (cause) { error = errorText(cause); }
  finally { busy = false; }
}

/** Submit an explicit patch, never a round-trip copy of a redacted credential document. */
async function applyPatch() {
  if (!section || !native?.writable) return;
  busy = true; error = null;
  try {
    const parsed: unknown = JSON.parse(patch);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error(t('agents.dsh_patch_object'));
    await api.updateDshSettings(section.ns, parsed as Record<string, unknown>, section.revision);
    native = await api.getDshSettings();
    patch = '{}';
    toasts.ok(t('settings.saved_toast'));
  } catch (cause) { error = errorText(cause); }
  finally { busy = false; }
}
</script>

<Section title="deepseek-harness" hint={t('agents.dsh_hint')}>
  <div class="grid gap-3 sm:grid-cols-2">
    <label><span class="label">{t('agents.dsh_url')}</span><TextField bind:value={url} disabled={busy} /></label>
    <label><span class="label">{t('agents.dsh_cookie')}</span><TextField bind:value={cookie} disabled={busy} /></label>
  </div>
  <div class="flex flex-wrap items-center gap-3">
    <Button onclick={() => void save()} disabled={busy}>{t('common.save')}</Button>
    <Button onclick={() => void loadNative()} disabled={busy || !connection}>{t('agents.dsh_load')}</Button>
    {#if connection}
      <a href={connection.base_url} target="_blank" rel="noreferrer" class="text-accent">{t('agents.dsh_open')}</a>
    {/if}
  </div>
  {#if error}<p class="notice notice-warn" role="alert">{error}</p>{/if}
  {#if native}
    <label><span class="label">{t('agents.dsh_namespace')}</span>
      <Select bind:value={namespace} disabled={busy} onchange={() => { patch = '{}'; }}>
        {#each native.namespaces as entry (entry.ns)}<option value={entry.ns}>{entry.ns}</option>{/each}
      </Select>
    </label>
    {#if section}
      <details><summary>{t('agents.dsh_current')}</summary><pre class="overflow-auto text-sm">{JSON.stringify(section.value, null, 2)}</pre></details>
      <label><span class="label">{t('agents.dsh_patch')}</span>
        <textarea class="input font-mono" rows="5" bind:value={patch} disabled={busy || !native.writable}></textarea>
      </label>
      <p class="hint">{t('agents.dsh_patch_hint')}</p>
      <Button disabled={busy || !native.writable || patch.trim() === '{}'} onclick={() => void applyPatch()}>{t('common.save')}</Button>
    {/if}
  {/if}
</Section>
