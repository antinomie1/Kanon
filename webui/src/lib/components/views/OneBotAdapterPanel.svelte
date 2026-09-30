<script lang="ts">
import {
  AlertTriangle,
  CheckCircle2,
  Eye,
  EyeOff,
  Radio,
  RefreshCw,
  Save,
  XCircle,
} from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { onebotStore as store } from '../../stores/onebot.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * Configuration panel for the built-in OneBot adapter.
 *
 * Everything here is a *node* setting: saving validates, persists to `data/system.json` and
 * reconnects the running adapter, so the panel never needs to tell the operator to restart.
 *
 * The panel is the body of the adapter's configuration drawer, which supplies the surface title
 * and the close affordance; it therefore renders its own state badge and actions only.
 */

let showToken = $state(false);
</script>

<div
  class="p-5 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 shadow-2xs space-y-4"
>
  <div class="flex flex-wrap items-center justify-between gap-3">
    <div class="flex items-center gap-2">
      <Radio class="w-4.5 h-4.5 text-zinc-500" />
      {#if store.status}
        <span
          class="px-2 py-0.5 rounded text-xs font-mono border {store.stateTone === 'ok'
            ? 'bg-emerald-500/10 text-emerald-600 border-emerald-500/20'
            : store.stateTone === 'warn'
              ? 'bg-amber-500/10 text-amber-600 border-amber-500/20'
              : store.stateTone === 'bad'
                ? 'bg-rose-500/10 text-rose-600 border-rose-500/20'
                : 'bg-zinc-500/10 text-zinc-500 border-zinc-500/20'}"
        >
          {t(store.stateLabelKey)}
        </span>
      {/if}
    </div>

    <button
      onclick={() => store.ensureLoaded()}
      class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1"
    >
      <RefreshCw class="w-3.5 h-3.5" />
      <span>{t('common.refresh')}</span>
    </button>
  </div>

  {#if store.unavailable}
    <div
      class="p-3 rounded-lg bg-zinc-100 dark:bg-zinc-800 text-xs text-zinc-600 dark:text-zinc-300 flex items-center gap-2"
    >
      <AlertTriangle class="w-4 h-4 shrink-0" />
      <span>{t('adapters.onebot_not_hosted')}</span>
    </div>
  {:else}
    <!-- Connection settings -->
    <div class="grid grid-cols-1 md:grid-cols-2 gap-3 text-xs sm:text-sm">
      <div class="md:col-span-2 flex items-center justify-between gap-3 p-3 rounded-lg bg-zinc-50 dark:bg-zinc-950/50 border border-zinc-100 dark:border-zinc-800">
        <div>
          <span class="font-medium text-zinc-800 dark:text-zinc-200">{t('adapters.onebot_enabled')}</span>
          <p class="text-xs text-zinc-500 mt-0.5">{t('adapters.onebot_enabled_hint')}</p>
          {#if store.hasPendingToggle}
            <p class="text-xs text-amber-600 dark:text-amber-400 mt-0.5">
              {t('adapters.onebot_unsaved')}
            </p>
          {/if}
        </div>
        <Switch
          checked={store.formEnabled}
          onchange={(next) => (store.formEnabled = next)}
          label={t('adapters.onebot_enabled')}
        />
      </div>

      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1"
          >{t('adapters.onebot_ws_url')}</label
        >
        <input
          type="text"
          bind:value={store.formWsUrl}
          placeholder="ws://127.0.0.1:6700"
          class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden focus:border-zinc-400 dark:focus:border-zinc-600"
        />
      </div>

      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1"
          >{t('adapters.onebot_transport')}</label
        >
        <select
          bind:value={store.formTransport}
          class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
        >
          <option value="forward_websocket">{t('adapters.onebot_transport_forward')}</option>
          <option value="reverse_websocket">{t('adapters.onebot_transport_reverse')}</option>
        </select>
      </div>

      <p class="md:col-span-2 text-xs text-zinc-500 dark:text-zinc-400">
        {store.formTransport === 'reverse_websocket'
          ? t('adapters.onebot_reverse_hint')
          : t('adapters.onebot_forward_hint')}
      </p>

      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1">
          {t('adapters.onebot_token')}
        </label>
        <div class="relative">
          <input
            type={showToken ? 'text' : 'password'}
            bind:value={store.formToken}
            placeholder={store.tokenConfigured
              ? t('adapters.onebot_token_keep')
              : t('adapters.onebot_token_none')}
            autocomplete="new-password"
            class="w-full px-3 py-2 pr-9 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
          />
          <button
            type="button"
            onclick={() => (showToken = !showToken)}
            class="absolute right-2 top-1/2 -translate-y-1/2 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 cursor-pointer"
            title={showToken ? t('adapters.onebot_token_hide') : t('adapters.onebot_token_show')}
          >
            {#if showToken}
              <EyeOff class="w-4 h-4" />
            {:else}
              <Eye class="w-4 h-4" />
            {/if}
          </button>
        </div>
        {#if store.tokenConfigured}
          <label class="flex items-center gap-2 mt-1.5 text-xs text-zinc-500 cursor-pointer">
            <input type="checkbox" bind:checked={store.formClearToken} class="cursor-pointer" />
            <span>{t('adapters.onebot_clear_token')}</span>
          </label>
        {/if}
      </div>

      <div class="grid grid-cols-2 gap-3">
        <div>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1"
            >{t('adapters.onebot_platform')}</label
          >
          <input
            type="text"
            value={store.config?.platform ?? ''}
            readonly
            class="w-full px-3 py-2 rounded-lg bg-zinc-100 dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm text-zinc-500 cursor-not-allowed"
          />
        </div>
        <div>
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1"
            >{t('adapters.onebot_display_name')}</label
          >
          <input
            type="text"
            value={store.config?.display_name ?? store.config?.platform ?? ''}
            readonly
            class="w-full px-3 py-2 rounded-lg bg-zinc-100 dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm text-zinc-500 cursor-not-allowed"
          />
        </div>
      </div>

      <p class="md:col-span-2 text-xs text-zinc-500 dark:text-zinc-400">
        {t('adapters.onebot_readonly_hint')}
      </p>
    </div>

    <!-- Actions -->
    <div class="flex flex-wrap items-center gap-2">
      <button
        onclick={() => store.save()}
        disabled={store.saving}
        class="px-3 py-2 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 text-white dark:text-zinc-900 rounded-lg text-xs sm:text-sm font-medium transition cursor-pointer disabled:opacity-50 flex items-center gap-1.5"
      >
        <Save class="w-3.5 h-3.5" />
        <span>{store.saving ? t('adapters.onebot_saving') : t('adapters.onebot_save')}</span>
      </button>
      {#if store.message === 'saved'}
        <span class="text-xs text-emerald-600 flex items-center gap-1">
          <CheckCircle2 class="w-3.5 h-3.5" />
          <span>{t('adapters.onebot_saved')}</span>
        </span>
      {/if}
    </div>

    {#if store.error}
      <div
        class="p-3 rounded-lg bg-rose-500/10 border border-rose-500/20 text-xs text-rose-700 dark:text-rose-300 flex items-start gap-2"
      >
        <XCircle class="w-4 h-4 shrink-0 mt-0.5" />
        <span class="font-mono break-all">{store.error}</span>
      </div>
    {/if}

    <!-- Live status -->
    {#if store.status}
      <div class="p-3 rounded-lg bg-zinc-50 dark:bg-zinc-950/50 border border-zinc-100 dark:border-zinc-800 text-xs">
        <span class="text-zinc-400 block mb-1">{t('adapters.onebot_login')}</span>
        <span class="font-mono text-zinc-900 dark:text-zinc-100">{store.status.self_id ?? '—'}</span>
      </div>

      {#if store.status.last_error}
        <div
          class="p-3 rounded-lg bg-amber-500/10 border border-amber-500/20 text-xs text-amber-700 dark:text-amber-300 flex items-start gap-2"
        >
          <AlertTriangle class="w-4 h-4 shrink-0 mt-0.5" />
          <span class="font-mono break-all">{store.status.last_error}</span>
        </div>
      {/if}
    {/if}

  {/if}
</div>
