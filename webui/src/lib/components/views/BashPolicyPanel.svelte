<script lang="ts">
import { onMount } from 'svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import type {
  BashLocalConfig,
  BashPolicy,
  BashSandboxConfig,
} from '../../types';

let enabled = $state(false);
let loaded = $state(false);
let saving = $state(false);
let error = $state('');
let saved = $state(false);
let sandbox = $state<BashSandboxConfig | null>(null);
let executionMode = $state<BashPolicy['execution_mode']>('sandbox');
let local = $state<BashLocalConfig | null>(null);
let reviewModel = $state('');

onMount(async () => {
  try {
    const policy = await api.getBashPolicy();
    enabled = policy.enabled;
    sandbox = policy.sandbox;
    executionMode = policy.execution_mode;
    local = policy.local;
    reviewModel = policy.local.review_model ?? '';
    loaded = true;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  }
});

async function save() {
  if (!sandbox || !local) return;
  saving = true;
  error = '';
  saved = false;
  try {
    await api.setBashPolicy({
      enabled,
      sandbox,
      execution_mode: executionMode,
      local: { ...local, review_model: reviewModel.trim() || null },
    });
    saved = true;
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    saving = false;
  }
}

async function resetSandbox() {
  if (!window.confirm(t('bash.reset_confirm'))) return;
  saving = true;
  error = '';
  try {
    await api.resetBashSandbox();
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    saving = false;
  }
}
</script>

<!-- Node-wide Bash settings. Who may use it comes from each instance's command permissions, and
     where from each instance's Bash scope (Instances → edit). -->
<form
  onsubmit={(event) => {
    event.preventDefault();
    void save();
  }}
  class="bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl p-6 shadow-xs"
>
  <div class="pb-4 border-b border-zinc-100 dark:border-zinc-800">
    <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">{t('bash.title')}</h3>
    <p class="text-xs text-zinc-500 mt-0.5">{t('bash.hint')}</p>
  </div>

  <div class="mt-5 space-y-4">
    <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
      <span>
        <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.enabled')}</span>
        <span class="block text-xs text-zinc-500 mt-0.5">{t('bash.identity_hint')}</span>
      </span>
      <input
        type="checkbox"
        bind:checked={enabled}
        disabled={!loaded || saving}
        onchange={() => { saved = false; }}
        class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
      />
    </label>

    <label class="block space-y-1.5">
      <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.execution_mode')}</span>
      <select
        bind:value={executionMode}
        disabled={!loaded || saving}
        onchange={() => { saved = false; }}
        class="w-full px-3 py-2 text-sm bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 rounded-lg cursor-pointer"
      >
        <option value="sandbox">{t('bash.mode_sandbox')}</option>
        <option value="local">{t('bash.mode_local')}</option>
      </select>
    </label>

    {#if executionMode === 'local' && local}
      <div class="space-y-3 border-t border-zinc-100 dark:border-zinc-800 pt-4">
        <p class="text-xs text-zinc-500">{t('bash.local_hint')}</p>
        <label class="block space-y-1.5">
          <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.local_workdir')}</span>
          <input
            bind:value={local.working_dir}
            disabled={!loaded || saving}
            oninput={() => { saved = false; }}
            class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs focus:outline-hidden"
          />
        </label>
        <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
          <span>
            <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.auto_review')}</span>
            {#if local.auto_review}
              <span class="block text-xs text-zinc-500 mt-0.5">{t('bash.review_hint')}</span>
            {/if}
          </span>
          <input
            type="checkbox"
            bind:checked={local.auto_review}
            disabled={!loaded || saving}
            onchange={() => { saved = false; }}
            class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
          />
        </label>
        {#if local.auto_review}
          <label class="block space-y-1.5">
            <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.review_model')}</span>
            <input
              bind:value={reviewModel}
              disabled={!loaded || saving}
              oninput={() => { saved = false; }}
              class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs focus:outline-hidden"
            />
          </label>
        {/if}
      </div>
    {/if}

    {#if executionMode === 'sandbox' && sandbox}
      <div class="space-y-3 border-t border-zinc-100 dark:border-zinc-800 pt-4">
        <span class="block text-sm font-semibold text-zinc-800 dark:text-zinc-200">{t('bash.sandbox_title')}</span>
        <p class="text-xs text-zinc-500">{t('bash.sandbox_hint')}</p>
        <label class="flex items-start justify-between gap-4 cursor-pointer select-none">
          <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.network')}</span>
          <input
            type="checkbox"
            bind:checked={sandbox.network}
            disabled={!loaded || saving}
            onchange={() => { saved = false; }}
            class="mt-1 rounded text-indigo-600 focus:ring-0 w-4 h-4 shrink-0"
          />
        </label>
        <label class="block space-y-1.5">
          <span class="block text-sm font-medium text-zinc-800 dark:text-zinc-200">{t('bash.image')}</span>
          <input
            bind:value={sandbox.image}
            disabled={!loaded || saving}
            oninput={() => { saved = false; }}
            class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs focus:outline-hidden"
          />
        </label>
        <div class="flex flex-wrap items-center justify-between gap-3">
          <span class="text-xs font-mono text-zinc-500">
            {sandbox.memory_mb} MiB · {sandbox.cpus} CPU · {sandbox.pids_limit} {t('bash.processes')}
          </span>
          <button
            type="button"
            onclick={resetSandbox}
            disabled={!loaded || saving}
            class="px-3 py-1.5 text-xs border border-zinc-200 dark:border-zinc-700 rounded-md hover:bg-zinc-100 dark:hover:bg-zinc-800 disabled:opacity-50 cursor-pointer"
          >
            {t('bash.reset')}
          </button>
        </div>
      </div>
    {/if}
  </div>

  {#if error}<p role="alert" class="text-xs text-rose-500 mt-4 font-mono">{error}</p>{/if}
  {#if saved}<p role="status" class="text-xs text-emerald-500 mt-4 font-mono">{t('bash.saved')}</p>{/if}

  <div class="mt-5 flex justify-end">
    <button
      type="submit"
      disabled={!loaded || saving}
      class="px-4 py-1.5 bg-zinc-900 dark:bg-zinc-100 hover:bg-zinc-700 dark:hover:bg-zinc-200 disabled:opacity-50 text-white dark:text-zinc-950 rounded-md text-xs font-medium transition cursor-pointer"
    >
      {saving ? t('models.saving') : t('common.save')}
    </button>
  </div>
</form>
