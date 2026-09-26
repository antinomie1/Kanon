<script lang="ts">
import { QrCode, Radio, RefreshCw, Send, Settings } from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import { milkyStore } from '../../stores/milky.svelte';
import type { AdapterItem } from '../../types';
import QqOfficialQrModal from '../adapters/QqOfficialQrModal.svelte';
import PluginConfigDrawer from '../plugins/PluginConfigDrawer.svelte';
import Switch from '../ui/Switch.svelte';
import MilkyAdapterPanel from './MilkyAdapterPanel.svelte';

/**
 * Platform adapters tab.
 *
 * Adapters are the node's platform boundary: every message that enters or leaves the microkernel
 * passes one of them. They answer a different question than the plugins tab — "which platforms can
 * this node talk to, and are they healthy?" against "what does the bot do" — so they get their own
 * tab beside tools, MCP servers and skills. Both tabs render the same plugin configuration drawer,
 * so a QQ Official adapter is configured identically from either one.
 */

let adapters = $state<AdapterItem[]>([]);
let loading = $state(false);
let error = $state<string | null>(null);

/** Milky has an account-level configuration surface of its own. */
let milkyConfigOpen = $state(false);
/** Plugin configuration drawer, used by adapters that are implemented as plugins. */
let configPluginId = $state<string | null>(null);
/** QQ Official QR binding dialog. */
let qrModalOpen = $state(false);

// Quick ingest test form.
let testPlatform = $state('webhook');
let testChannel = $state('general');
let testSender = $state('alice');
let testMessage = $state('Hello Kanon!');
let ingesting = $state(false);
let ingestResult = $state<string | null>(null);

/** Loads the adapter catalog and keeps the Milky store in step with it. */
async function load() {
  loading = true;
  error = null;
  try {
    const res = await api.getAdapters();
    adapters = res.adapters;
    if (
      adapters.length > 0 &&
      !adapters.some((adapter) => adapter.platform === testPlatform)
    ) {
      // Keep the simulator pointed at a platform the node actually serves.
      testPlatform = adapters[0].platform;
    }
    await milkyStore.ensureLoaded();
  } catch (e) {
    error = e instanceof Error ? e.message : String(e);
  } finally {
    loading = false;
  }
}

/** Opens the Milky adapter's configuration drawer, reloading what the node currently has. */
function openMilkyConfig() {
  milkyConfigOpen = true;
  void milkyStore.load();
}

/** Pushes one synthetic event into a platform's Fast-ACK ingest endpoint. */
async function sendTestEvent() {
  ingesting = true;
  ingestResult = null;
  try {
    const res = await api.ingestEvent(testPlatform, {
      channel_id: testChannel,
      sender_id: testSender,
      text: testMessage,
      event_id: `test_${Date.now()}`,
    });
    ingestResult = `Fast-ACK Accepted! Event ID: ${res.event_id}`;
  } catch (e) {
    ingestResult = `Ingest rejected: ${e instanceof Error ? e.message : String(e)}`;
  } finally {
    ingesting = false;
  }
}

$effect(() => {
  void load();
});
</script>

<div class="space-y-6">
  <div
    class="p-5 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shadow-2xs space-y-3.5"
  >
    <div class="flex items-center justify-between gap-3">
      <div class="flex items-center gap-2">
        <Radio class="w-4.5 h-4.5 text-zinc-500" />
        <h4 class="text-sm sm:text-base font-semibold text-zinc-900 dark:text-zinc-100">
          {t('plugins.adapters_title')} ({adapters.length})
        </h4>
      </div>
      <button
        onclick={() => load()}
        class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1"
      >
        <RefreshCw class="w-3.5 h-3.5" />
        <span>{t('common.refresh')}</span>
      </button>
    </div>

    {#if error}
      <div
        class="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-700 dark:text-rose-300 font-mono"
      >
        {error}
      </div>
    {/if}

    {#if adapters.length === 0 && !loading}
      <p class="text-xs text-zinc-500">{t('adapters.empty')}</p>
    {/if}

    <div class="space-y-2">
      {#each adapters as adapter (adapter.platform)}
        <div
          class="p-3.5 rounded-lg border border-zinc-200 dark:border-zinc-800 flex flex-col sm:flex-row sm:items-center justify-between gap-2 text-xs sm:text-sm"
        >
          <div class="flex items-center gap-2 flex-wrap">
            <span class="font-semibold text-zinc-800 dark:text-zinc-200">{adapter.display_name}</span>
            <span class="text-xs font-mono text-zinc-500">({adapter.platform})</span>
            {#if adapter.platform === milkyStore.platformId && milkyStore.status}
              <!-- The Milky adapter's own state is more precise than "connected or not": it
                   distinguishes disabled, connecting and failed, which need different reactions. -->
              <span
                class="px-2 py-0.5 rounded text-xs font-mono border {milkyStore.stateTone === 'ok'
                  ? 'bg-emerald-500/10 text-emerald-600 border-emerald-500/20'
                  : milkyStore.stateTone === 'warn'
                    ? 'bg-amber-500/10 text-amber-600 border-amber-500/20'
                    : milkyStore.stateTone === 'bad'
                      ? 'bg-rose-500/10 text-rose-600 border-rose-500/20'
                      : 'bg-zinc-500/10 text-zinc-500 border-zinc-500/20'}"
              >
                {t(milkyStore.stateLabelKey)}
              </span>
            {:else}
              <span
                class="px-2 py-0.5 rounded text-xs font-mono {adapter.connected
                  ? 'bg-emerald-500/10 text-emerald-600 border border-emerald-500/20'
                  : 'bg-zinc-500/10 text-zinc-500 border border-zinc-500/20'}"
              >
                {adapter.connected ? 'Active' : 'Inbound-only'}
              </span>
            {/if}
          </div>

          <div class="flex items-center gap-2 self-end sm:self-auto">
            {#if adapter.platform === 'qqofficial'}
              <button
                onclick={() => (qrModalOpen = true)}
                class="px-2.5 py-1 text-xs font-medium text-emerald-600 dark:text-emerald-400 bg-emerald-500/10 hover:bg-emerald-500/20 border border-emerald-500/20 rounded-md transition cursor-pointer flex items-center gap-1 shadow-2xs"
                title={t('adapters.qq_qr_title')}
              >
                <QrCode class="w-3.5 h-3.5" />
                <span>{t('adapters.qq_qr_btn')}</span>
              </button>
              <button
                onclick={() => (configPluginId = 'org.kanon.adapter.qqofficial')}
                class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1"
              >
                <Settings class="w-3.5 h-3.5" />
                <span>{t('plugins.config')}</span>
              </button>
            {:else if adapter.platform === milkyStore.platformId}
              <!-- Outside switch for the same node setting the panel edits: it applies
                   immediately, and the two stay in step because both go through the store. -->
              <Switch
                checked={milkyStore.status?.enabled ?? false}
                disabled={milkyStore.loading || milkyStore.applyingEnabled}
                onchange={(next) => void milkyStore.setEnabled(next)}
                label={t('adapters.milky_enabled')}
              />
              <button
                onclick={openMilkyConfig}
                class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1"
              >
                <Settings class="w-3.5 h-3.5" />
                <span>{t('plugins.config')}</span>
              </button>
            {/if}
          </div>
        </div>
      {/each}
    </div>

    {#if milkyStore.error && !milkyConfigOpen}
      <div
        class="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-700 dark:text-rose-300 font-mono break-all"
      >
        {milkyStore.error}
      </div>
    {/if}
  </div>

  <!-- Simulated inbound events: a debugging aid for the Fast-ACK data plane, kept at the bottom of
       the page so it never competes with the adapters themselves. -->
  <div
    class="p-5 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shadow-2xs space-y-3.5"
  >
    <div class="flex items-center gap-2">
      <Send class="w-4.5 h-4.5 text-zinc-500" />
      <h4 class="text-sm sm:text-base font-semibold text-zinc-900 dark:text-zinc-100">
        Simulate Inbound Event (Fast-ACK)
      </h4>
    </div>
    <div class="space-y-3 text-xs sm:text-sm">
      <div class="grid grid-cols-3 gap-2.5">
        <div>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="block text-xs font-sans text-zinc-500 mb-1">Platform</label>
          <input
            type="text"
            bind:value={testPlatform}
            class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
          />
        </div>
        <div>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="block text-xs font-sans text-zinc-500 mb-1">Channel ID</label>
          <input
            type="text"
            bind:value={testChannel}
            class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
          />
        </div>
        <div>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="block text-xs font-sans text-zinc-500 mb-1">Sender ID</label>
          <input
            type="text"
            bind:value={testSender}
            class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
          />
        </div>
      </div>
      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs font-sans text-zinc-500 mb-1">Message Content</label>
        <input
          type="text"
          bind:value={testMessage}
          class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 text-xs sm:text-sm focus:outline-hidden"
        />
      </div>
      <button
        onclick={sendTestEvent}
        disabled={ingesting}
        class="w-full py-2 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 text-white dark:text-zinc-900 rounded-lg font-medium transition cursor-pointer text-xs sm:text-sm disabled:opacity-50"
      >
        {ingesting ? 'Pushing into Tokio queue...' : 'Send Event to Pipeline'}
      </button>
      {#if ingestResult}
        <div
          class="p-2.5 rounded-lg bg-zinc-100 dark:bg-zinc-800 font-mono text-xs text-zinc-700 dark:text-zinc-300"
        >
          {ingestResult}
        </div>
      {/if}
    </div>
  </div>
</div>

<!-- Milky adapter configuration drawer: the built-in adapter's second-level view, mirroring how
     the QQ Official plugin exposes its own settings from the adapter list. -->
{#if milkyConfigOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (milkyConfigOpen = false)}
    role="button"
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-3xl max-h-[85vh] overflow-y-auto bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl p-6 space-y-4"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div
        class="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800 pb-3"
      >
        <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100">
          {t('adapters.milky_title')}
        </h3>
        <button
          onclick={() => (milkyConfigOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-xs sm:text-sm font-mono cursor-pointer"
        >
          {t('common.close')}
        </button>
      </div>

      <MilkyAdapterPanel />
    </div>
  </div>
{/if}

<QqOfficialQrModal
  open={qrModalOpen}
  onclose={() => (qrModalOpen = false)}
  onbound={() => void load()}
/>

<PluginConfigDrawer
  pluginId={configPluginId}
  onclose={() => (configPluginId = null)}
  onrefresh={() => void load()}
/>
