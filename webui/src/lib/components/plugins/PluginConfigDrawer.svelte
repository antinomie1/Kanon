<script lang="ts">
import { Save } from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import type { PluginConfigResponse } from '../../types';

/**
 * Configuration drawer for one discovered plugin.
 *
 * Plugin configuration is generic JSON validated against the manifest's schema. Built-in platform
 * adapters are configured on the adapters tab instead, so no plugin needs a hand-written form.
 */
let {
  pluginId,
  onclose,
}: {
  /** Plugin whose configuration is shown; `null` keeps the drawer closed. */
  pluginId: string | null;
  /** Called when the operator dismisses the drawer. */
  onclose: () => void;
} = $props();

let currentConfig = $state<PluginConfigResponse | null>(null);
let configEditRaw = $state<string>('');
let configSaving = $state(false);
let configStatusMsg = $state<string | null>(null);

/** Loads the plugin's current configuration into the editor. */
async function loadConfig(id: string) {
  configStatusMsg = null;
  try {
    const res = await api.getPluginConfig(id);
    currentConfig = res;
    configEditRaw = JSON.stringify(res.values, null, 2);
  } catch (e) {
    currentConfig = null;
    configStatusMsg = `Failed to fetch config: ${e instanceof Error ? e.message : String(e)}`;
  }
}

/** Saves the document, enforcing the compare-and-swap version the node reported. */
async function saveConfig() {
  if (!pluginId || !currentConfig) return;
  configSaving = true;
  configStatusMsg = null;
  try {
    const parsed = JSON.parse(configEditRaw);
    const res = await api.updatePluginConfig(
      pluginId,
      parsed,
      currentConfig.version,
    );
    currentConfig.version = res.version;
    currentConfig.values = res.values;
    configStatusMsg = 'Configuration saved successfully (CAS enforced).';
  } catch (e) {
    configStatusMsg = `Save failed: ${e instanceof Error ? e.message : String(e)}`;
  } finally {
    configSaving = false;
  }
}

// Reload whenever the drawer is pointed at a different plugin.
$effect(() => {
  if (pluginId) {
    void loadConfig(pluginId);
  }
});
</script>

<!-- Plugin configuration drawer -->
{#if pluginId}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={onclose}
    role="button"
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-2xl bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800 pb-3">
        <div>
          <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">Plugin Config: {pluginId}</h3>
          <span class="text-xs font-mono text-zinc-500">{t('plugins.cas_version')}: {currentConfig?.version ?? 0}</span>
        </div>
        <button
          onclick={onclose}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-xs sm:text-sm font-mono cursor-pointer"
        >
          {t('common.close')}
        </button>
      </div>

      {#if configStatusMsg}
        <div class="p-3 text-xs sm:text-sm rounded-lg bg-zinc-100 dark:bg-zinc-800 font-mono text-zinc-700 dark:text-zinc-300">
          {configStatusMsg}
        </div>
      {/if}

      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs sm:text-sm font-medium text-zinc-600 dark:text-zinc-400 mb-1.5">Configuration (JSON)</label>
        <textarea
          bind:value={configEditRaw}
          rows={10}
          class="w-full p-3 bg-zinc-950 font-mono text-xs sm:text-sm text-zinc-200 border border-zinc-800 rounded-lg focus:outline-hidden"
        ></textarea>
      </div>

      <div class="flex items-center justify-end gap-2 pt-2">
        <button
          onclick={onclose}
          class="px-3.5 py-2 text-xs sm:text-sm text-zinc-600 dark:text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800 rounded-lg transition cursor-pointer"
        >
          {t('common.cancel')}
        </button>
        <button
          onclick={saveConfig}
          disabled={configSaving}
          class="px-4 py-2 text-xs sm:text-sm bg-indigo-600 hover:bg-indigo-500 text-white rounded-lg font-medium transition cursor-pointer flex items-center gap-1.5 disabled:opacity-50"
        >
          <Save class="w-4 h-4" />
          <span>{configSaving ? 'Enforcing CAS...' : t('common.save')}</span>
        </button>
      </div>
    </div>
  </div>
{/if}
