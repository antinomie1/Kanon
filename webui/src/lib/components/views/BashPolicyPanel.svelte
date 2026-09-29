<script lang="ts">
import { onMount } from 'svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import type { BashPolicy, BashPrincipal, BashSandboxConfig } from '../../types';

let mode = $state<BashPolicy['mode']>('allowlist');
let allowlist = $state('');
let denylist = $state('');
let loaded = $state(false);
let saving = $state(false);
let error = $state('');
let saved = $state(false);
let sandbox = $state<BashSandboxConfig | null>(null);

/** One platform:user identity per line; split once so platform-scoped ids remain intact. */
function parseEntries(text: string): BashPrincipal[] {
  return text
    .split('\n')
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const separator = line.indexOf(':');
      if (separator <= 0 || separator === line.length - 1) {
        throw new Error(t('bash.invalid_identity'));
      }
      return {
        platform: line.slice(0, separator).trim(),
        user_id: line.slice(separator + 1).trim(),
      };
    });
}

onMount(async () => {
  try {
    const policy = await api.getBashPolicy();
    mode = policy.mode;
    sandbox = policy.sandbox;
    allowlist = policy.allowlist
      .map((entry) => `${entry.platform}:${entry.user_id}`)
      .join('\n');
    denylist = policy.denylist
      .map((entry) => `${entry.platform}:${entry.user_id}`)
      .join('\n');
    loaded = true;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }
});

async function save() {
  if (!sandbox) return;
  saving = true;
  error = '';
  saved = false;
  try {
    await api.setBashPolicy({
      sandbox,
      mode,
      allowlist: parseEntries(allowlist),
      denylist: parseEntries(denylist),
    });
    saved = true;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    saving = false;
  }
}
</script>

<form onsubmit={(event) => { event.preventDefault(); void save(); }} class="rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-4 space-y-3">
  <h3 class="text-sm font-semibold">{t('bash.title')}</h3>
  <p class="text-xs text-zinc-500">{t('bash.hint')}</p>
  <label class="block text-sm space-y-1">
    <span>{t('bash.mode')}</span>
    <select bind:value={mode} disabled={!loaded || saving} onchange={() => { saved = false; }} class="block w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-transparent p-2">
      <option value="allowlist">{t('bash.allowlist_mode')}</option>
      <option value="denylist">{t('bash.denylist_mode')}</option>
    </select>
  </label>
  <div class="grid gap-3 sm:grid-cols-2">
    <label class="block text-sm space-y-1">
      <span>{t('bash.allowlist')}</span>
      <textarea bind:value={allowlist} disabled={!loaded || saving} oninput={() => { saved = false; }} rows="3" placeholder="onebot:123456" class="block w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-transparent p-2 font-mono text-xs"></textarea>
    </label>
    <label class="block text-sm space-y-1">
      <span>{t('bash.denylist')}</span>
      <textarea bind:value={denylist} disabled={!loaded || saving} oninput={() => { saved = false; }} rows="3" placeholder="onebot:654321" class="block w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-transparent p-2 font-mono text-xs"></textarea>
    </label>
  </div>
  <p class="text-xs text-zinc-500">{t('bash.identity_hint')}</p>
  {#if sandbox}
    <div class="space-y-2 border-t border-zinc-200 dark:border-zinc-800 pt-3">
      <h4 class="text-sm font-semibold">{t('bash.sandbox_title')}</h4>
      <label class="flex items-center gap-2 text-sm">
        <input type="checkbox" bind:checked={sandbox.network} disabled={!loaded || saving} onchange={() => { saved = false; }} />
        <span>{t('bash.network')}</span>
      </label>
      <p class="text-xs text-zinc-500">{t('bash.sandbox_hint')}</p>
      <label class="block text-sm space-y-1">
        <span>{t('bash.image')}</span>
        <input bind:value={sandbox.image} disabled={!loaded || saving} oninput={() => { saved = false; }} class="block w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-transparent p-2 font-mono text-xs" />
      </label>
      <p class="text-xs text-zinc-500">{sandbox.memory_mb} MiB · {sandbox.cpus} CPU · {sandbox.pids_limit} {t('bash.processes')}</p>
    </div>
  {/if}
  {#if error}<p role="alert" class="text-sm text-rose-600">{error}</p>{/if}
  {#if saved}<p role="status" class="text-sm text-emerald-600">{t('bash.saved')}</p>{/if}
  <button type="submit" disabled={!loaded || saving} class="rounded-lg bg-indigo-600 px-3 py-2 text-sm text-white disabled:opacity-50">{saving ? t('common.loading') : t('common.save')}</button>
</form>
