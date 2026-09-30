<script lang="ts">
import { Radio, RefreshCw, Settings } from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';
import { milkyStore } from '../../stores/milky.svelte';
import { onebotStore } from '../../stores/onebot.svelte';
import { qqofficialStore } from '../../stores/qqofficial.svelte';
import type { AdapterItem } from '../../types';
import Switch from '../ui/Switch.svelte';
import MilkyAdapterPanel from './MilkyAdapterPanel.svelte';
import OneBotAdapterPanel from './OneBotAdapterPanel.svelte';
import QqOfficialAdapterPanel from './QqOfficialAdapterPanel.svelte';

/**
 * Platform adapters tab.
 *
 * Adapters are the node's platform boundary: every message that enters or leaves the microkernel
 * passes one of them. They answer a different question than the plugins tab — "which platforms can
 * this node talk to, and are they healthy?" against "what does the bot do" — so they get their own
 * tab beside tools, MCP servers and skills. Built-in adapters (Milky, OneBot v11, QQ Official) each
 * open their own configuration drawer from here.
 */

let adapters = $state<AdapterItem[]>([]);
let loading = $state(false);
let error = $state<string | null>(null);

/** Milky has an account-level configuration surface of its own. */
let milkyConfigOpen = $state(false);
/** OneBot v11 connection settings drawer. */
let onebotConfigOpen = $state(false);
/** QQ Official credentials drawer. */
let qqConfigOpen = $state(false);

/** Loads the adapter catalog and keeps the Milky store in step with it. */
async function load() {
  loading = true;
  error = null;
  try {
    const res = await api.getAdapters();
    adapters = res.adapters;
    await Promise.all([
      milkyStore.ensureLoaded(),
      onebotStore.ensureLoaded(),
      qqofficialStore.ensureLoaded(),
    ]);
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

/** Opens OneBot settings with the current saved values. */
function openOneBotConfig() {
  onebotConfigOpen = true;
  void onebotStore.load();
}

/** Opens QQ Official settings with the current saved values. */
function openQqConfig() {
  qqConfigOpen = true;
  void qqofficialStore.load();
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
            {:else if adapter.platform === qqofficialStore.platformId && qqofficialStore.status}
              <span
                class="px-2 py-0.5 rounded text-xs font-mono border {qqofficialStore.stateTone === 'ok'
                  ? 'bg-emerald-500/10 text-emerald-600 border-emerald-500/20'
                  : qqofficialStore.stateTone === 'warn'
                    ? 'bg-amber-500/10 text-amber-600 border-amber-500/20'
                    : qqofficialStore.stateTone === 'bad'
                      ? 'bg-rose-500/10 text-rose-600 border-rose-500/20'
                      : 'bg-zinc-500/10 text-zinc-500 border-zinc-500/20'}"
              >
                {t(qqofficialStore.stateLabelKey)}
              </span>
            {:else if adapter.platform === onebotStore.platformId && onebotStore.status}
              <!-- The OneBot adapter's own state is more precise than "connected or not": it
                   distinguishes disabled, connecting and failed, which need different reactions. -->
              <span
                class="px-2 py-0.5 rounded text-xs font-mono border {onebotStore.stateTone === 'ok'
                  ? 'bg-emerald-500/10 text-emerald-600 border-emerald-500/20'
                  : onebotStore.stateTone === 'warn'
                    ? 'bg-amber-500/10 text-amber-600 border-amber-500/20'
                    : onebotStore.stateTone === 'bad'
                      ? 'bg-rose-500/10 text-rose-600 border-rose-500/20'
                      : 'bg-zinc-500/10 text-zinc-500 border-zinc-500/20'}"
              >
                {t(onebotStore.stateLabelKey)}
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
            <!-- What the adapter declares it supports; settings elsewhere name adapters by these. -->
            {#each adapter.capabilities ?? [] as capability (capability)}
              <span
                class="px-1.5 py-0.5 rounded text-[10px] bg-zinc-100 dark:bg-zinc-800 text-zinc-500 dark:text-zinc-400"
              >
                {t(`capability.${capability}`)}
              </span>
            {/each}
          </div>

          <div class="flex items-center gap-2 self-end sm:self-auto">
            {#if adapter.platform === qqofficialStore.platformId}
              <Switch
                checked={qqofficialStore.status?.enabled ?? false}
                disabled={qqofficialStore.loading || qqofficialStore.applyingEnabled}
                onchange={(next) => void qqofficialStore.setEnabled(next)}
                label={t('adapters.qq_enabled')}
              />
              <button
                onclick={openQqConfig}
                class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1 whitespace-nowrap shrink-0"
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
                class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1 whitespace-nowrap shrink-0"
              >
                <Settings class="w-3.5 h-3.5" />
                <span>{t('plugins.config')}</span>
              </button>
            {:else if adapter.platform === onebotStore.platformId}
              <!-- Outside switch for the same node setting the panel edits: it applies
                   immediately, and the two stay in step because both go through the store. -->
              <Switch
                checked={onebotStore.status?.enabled ?? false}
                disabled={onebotStore.loading || onebotStore.applyingEnabled}
                onchange={(next) => void onebotStore.setEnabled(next)}
                label={t('adapters.onebot_enabled')}
              />
              <button
                onclick={openOneBotConfig}
                class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1 whitespace-nowrap shrink-0"
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
    {#if qqofficialStore.error && !qqConfigOpen}
      <div
        class="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-700 dark:text-rose-300 font-mono break-all"
      >
        {qqofficialStore.error}
      </div>
    {/if}
    {#if onebotStore.error && !onebotConfigOpen}
      <div
        class="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-700 dark:text-rose-300 font-mono break-all"
      >
        {onebotStore.error}
      </div>
    {/if}
  </div>

</div>

<!-- Milky adapter configuration drawer: the built-in adapter's second-level view, opened from the
     adapter list. -->
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

<!-- OneBot adapter configuration drawer: the built-in adapter's second-level view, opened from the
     adapter list. -->
{#if onebotConfigOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (onebotConfigOpen = false)}
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
          {t('adapters.onebot_title')}
        </h3>
        <button
          onclick={() => (onebotConfigOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-xs sm:text-sm font-mono cursor-pointer"
        >
          {t('common.close')}
        </button>
      </div>

      <OneBotAdapterPanel />
    </div>
  </div>
{/if}

<!-- QQ Official configuration drawer: credentials, delivery options and QR binding. -->
{#if qqConfigOpen}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={() => (qqConfigOpen = false)}
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
          {t('adapters.qq_title')}
        </h3>
        <button
          onclick={() => (qqConfigOpen = false)}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 text-xs sm:text-sm font-mono cursor-pointer"
        >
          {t('common.close')}
        </button>
      </div>

      <QqOfficialAdapterPanel />
    </div>
  </div>
{/if}
