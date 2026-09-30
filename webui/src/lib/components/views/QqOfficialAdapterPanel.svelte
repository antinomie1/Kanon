<script lang="ts">
import {
  AlertTriangle,
  CheckCircle2,
  Eye,
  EyeOff,
  QrCode,
  Radio,
  RefreshCw,
  Save,
  XCircle,
} from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { qqofficialStore as store } from '../../stores/qqofficial.svelte';
import QqOfficialQrModal from '../adapters/QqOfficialQrModal.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * Configuration panel for the built-in QQ Official adapter.
 *
 * Saving validates, persists to `data/system.json` and reconnects the gateway, so the operator
 * never needs a restart. Credentials can be typed in or bound by scanning a QR code; a QR binding
 * is applied and saved by the node itself, so the panel only reloads afterwards.
 *
 * The panel is the body of the adapter's configuration drawer, which supplies the title and the
 * close affordance.
 */

let showSecret = $state(false);
let qrOpen = $state(false);
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

    <div class="flex items-center gap-2">
      {#if !store.unavailable}
        <button
          onclick={() => (qrOpen = true)}
          class="px-2.5 py-1 text-xs font-medium text-emerald-600 dark:text-emerald-400 bg-emerald-500/10 hover:bg-emerald-500/20 border border-emerald-500/20 rounded-md transition cursor-pointer flex items-center gap-1"
          title={t('adapters.qq_qr_title')}
        >
          <QrCode class="w-3.5 h-3.5" />
          <span>{t('adapters.qq_qr_btn')}</span>
        </button>
      {/if}
      <button
        onclick={() => store.ensureLoaded()}
        class="px-2.5 py-1 text-xs font-medium text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-100 dark:hover:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded-md transition cursor-pointer flex items-center gap-1"
      >
        <RefreshCw class="w-3.5 h-3.5" />
        <span>{t('common.refresh')}</span>
      </button>
    </div>
  </div>

  {#if store.unavailable}
    <div
      class="p-3 rounded-lg bg-zinc-100 dark:bg-zinc-800 text-xs text-zinc-600 dark:text-zinc-300 flex items-center gap-2"
    >
      <AlertTriangle class="w-4 h-4 shrink-0" />
      <span>{t('adapters.qq_not_hosted')}</span>
    </div>
  {:else}
    <div class="grid grid-cols-1 md:grid-cols-2 gap-3 text-xs sm:text-sm">
      <div
        class="md:col-span-2 flex items-center justify-between gap-3 p-3 rounded-lg bg-zinc-50 dark:bg-zinc-950/50 border border-zinc-100 dark:border-zinc-800"
      >
        <div>
          <span class="font-medium text-zinc-800 dark:text-zinc-200">{t('adapters.qq_enabled')}</span>
          <p class="text-xs text-zinc-500 mt-0.5">{t('adapters.qq_enabled_hint')}</p>
          {#if store.hasPendingToggle}
            <p class="text-xs text-amber-600 dark:text-amber-400 mt-0.5">{t('adapters.qq_unsaved')}</p>
          {/if}
        </div>
        <Switch
          checked={store.formEnabled}
          onchange={(next) => (store.formEnabled = next)}
          label={t('adapters.qq_enabled')}
        />
      </div>

      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1"
          >{t('adapters.qq_appid')}</label
        >
        <input
          type="text"
          bind:value={store.formAppId}
          placeholder="102xxxxxx"
          class="w-full px-3 py-2 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden focus:border-zinc-400 dark:focus:border-zinc-600"
        />
      </div>

      <div>
        <!-- svelte-ignore a11y_label_has_associated_control -->
        <label class="block text-xs font-medium text-zinc-600 dark:text-zinc-400 mb-1"
          >{t('adapters.qq_secret')}</label
        >
        <div class="relative">
          <input
            type={showSecret ? 'text' : 'password'}
            bind:value={store.formSecret}
            placeholder={store.status?.secret_configured
              ? t('adapters.qq_secret_keep')
              : t('adapters.qq_secret_none')}
            autocomplete="new-password"
            class="w-full px-3 py-2 pr-9 rounded-lg bg-zinc-50 dark:bg-zinc-950 border border-zinc-200 dark:border-zinc-800 font-mono text-xs sm:text-sm focus:outline-hidden"
          />
          <button
            type="button"
            onclick={() => (showSecret = !showSecret)}
            class="absolute right-2 top-1/2 -translate-y-1/2 text-zinc-400 hover:text-zinc-700 dark:hover:text-zinc-200 cursor-pointer"
          >
            {#if showSecret}
              <EyeOff class="w-4 h-4" />
            {:else}
              <Eye class="w-4 h-4" />
            {/if}
          </button>
        </div>
      </div>

      <label class="flex items-center gap-2 cursor-pointer">
        <input type="checkbox" bind:checked={store.formMarkdown} class="cursor-pointer" />
        <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_use_markdown')}</span>
      </label>
      <label class="flex items-center gap-2 cursor-pointer">
        <input type="checkbox" bind:checked={store.formSandbox} class="cursor-pointer" />
        <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_sandbox')}</span>
      </label>
      <label class="flex items-center gap-2 cursor-pointer md:col-span-2">
        <input type="checkbox" bind:checked={store.formTypingIndicator} class="cursor-pointer" />
        <span class="text-xs text-zinc-700 dark:text-zinc-300">{t('adapters.qq_typing')}</span>
      </label>
    </div>

    <div class="flex flex-wrap items-center gap-2">
      <button
        onclick={() => store.save()}
        disabled={store.saving}
        class="px-3 py-2 bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 text-white dark:text-zinc-900 rounded-lg text-xs sm:text-sm font-medium transition cursor-pointer disabled:opacity-50 flex items-center gap-1.5"
      >
        <Save class="w-3.5 h-3.5" />
        <span>{store.saving ? t('adapters.qq_saving') : t('adapters.qq_save')}</span>
      </button>
      {#if store.message === 'saved'}
        <span class="text-xs text-emerald-600 flex items-center gap-1">
          <CheckCircle2 class="w-3.5 h-3.5" />
          <span>{t('adapters.qq_saved')}</span>
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

    {#if store.status}
      <div
        class="p-3 rounded-lg bg-zinc-50 dark:bg-zinc-950/50 border border-zinc-100 dark:border-zinc-800 text-xs"
      >
        <span class="text-zinc-400 block mb-1">{t('adapters.qq_bot')}</span>
        <span class="font-mono text-zinc-900 dark:text-zinc-100">{store.status.bot_name ?? '—'}</span>
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

<QqOfficialQrModal open={qrOpen} onclose={() => (qrOpen = false)} onbound={() => void store.load()} />
