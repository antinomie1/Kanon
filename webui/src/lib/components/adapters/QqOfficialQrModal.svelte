<script lang="ts">
import {
  AlertTriangle,
  Check,
  CheckCircle2,
  Copy,
  ExternalLink,
  QrCode,
  RefreshCw,
  X,
  XCircle,
} from 'lucide-svelte';
import { api } from '../../api/client';
import { t } from '../../stores/i18n.svelte';

/**
 * QR-code binding flow for the QQ Official adapter.
 *
 * The modal is a self-contained flow: it requests a login task, polls it until the operator
 * authorizes (or it expires) and reports the credentials back to its caller. It is rendered by
 * both the platform-adapters page and the plugin configuration drawer, so it owns no state that
 * either caller needs to reason about — only `open`, `onclose` and the bind result.
 */
let {
  open = false,
  onclose,
  onbound,
}: {
  open?: boolean;
  onclose: () => void;
  /** Called after a successful authorization so the caller can adopt the credentials. */
  onbound?: (credentials: { appid: string | null; secret: string | null }) => void;
} = $props();

let qrTaskId = $state<string | null>(null);
let qrBindKey = $state<string | null>(null);
let qrCodeUrl = $state<string | null>(null);
let qrStatus = $state<'idle' | 'generating' | 'waiting' | 'success' | 'expired' | 'error'>('idle');
let qrStatusMsg = $state<string | null>(null);
let qrBoundAppId = $state<string | null>(null);
let qrCopied = $state(false);
// Deliberately not reactive: the timer is an implementation detail and reading a `$state` handle
// from the open/close effect would make the effect depend on its own writes.
let qrPollTimer: ReturnType<typeof setInterval> | null = null;

/**
 * Requests a fresh login task and starts polling it.
 *
 * Every entry point (opening the dialog or pressing retry after a failure) restarts from a clean
 * slate, so a stale task can never be polled by accident.
 */
async function startLogin() {
  qrStatus = 'generating';
  qrStatusMsg = null;
  qrTaskId = null;
  qrBindKey = null;
  qrCodeUrl = null;
  qrBoundAppId = null;
  qrCopied = false;
  stopQrPolling();

  try {
    const res = await api.requestQQOfficialLoginQr();
    qrTaskId = res.task_id;
    qrBindKey = res.bind_key;
    qrCodeUrl = res.qrcode_url;
    qrStatus = 'waiting';

    const intervalMs = Math.max(res.poll_interval_seconds || 2, 1) * 1000;
    qrPollTimer = setInterval(async () => {
      if (!qrTaskId || !qrBindKey) return;
      try {
        const pollRes = await api.pollQQOfficialLogin(qrTaskId, qrBindKey);
        if (pollRes.status === 'created') {
          stopQrPolling();
          qrStatus = 'success';
          qrBoundAppId = pollRes.appid ?? '';
          onbound?.({ appid: pollRes.appid ?? null, secret: pollRes.secret ?? null });
        } else if (pollRes.status === 'expired') {
          stopQrPolling();
          qrStatus = 'expired';
        } else if (pollRes.status === 'error') {
          stopQrPolling();
          qrStatus = 'error';
          qrStatusMsg = pollRes.message || 'Error polling authorization status';
        }
      } catch {
        // Keep polling on transient network glitch
      }
    }, intervalMs);
  } catch (e) {
    qrStatus = 'error';
    qrStatusMsg = e instanceof Error ? e.message : String(e);
  }
}

/** Stops the polling interval, if one is running. */
function stopQrPolling() {
  if (qrPollTimer) {
    clearInterval(qrPollTimer);
    qrPollTimer = null;
  }
}

/** Copies the authorization URL for clients that cannot show a QR code. */
async function copyQrUrl() {
  if (!qrCodeUrl) return;
  try {
    await navigator.clipboard.writeText(qrCodeUrl);
    qrCopied = true;
    setTimeout(() => {
      qrCopied = false;
    }, 2000);
  } catch {
    // clipboard
  }
}

// Opening starts the flow and closing always stops the poll: a poll left running would keep
// hitting the node for a dialog nobody is looking at.
$effect(() => {
  if (open) {
    void startLogin();
  } else {
    stopQrPolling();
  }
  return () => stopQrPolling();
});
</script>

<!-- QQ Official QR Code Login Modal -->
{#if open}
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/50 backdrop-blur-xs z-50 flex items-center justify-center p-4"
    onclick={onclose}
    role="button"
    tabindex="-1"
  >
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-md bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-2xl shadow-2xl p-6 space-y-5 text-center"
      onclick={(e) => e.stopPropagation()}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center justify-between border-b border-zinc-200 dark:border-zinc-800 pb-3 text-left">
        <div>
          <h3 class="text-base font-semibold text-zinc-900 dark:text-zinc-100 flex items-center gap-2">
            <QrCode class="w-5 h-5 text-emerald-600 dark:text-emerald-400" />
            <span>{t('adapters.qq_qr_title')}</span>
          </h3>
          <p class="text-xs text-zinc-500 mt-0.5">{t('adapters.qq_qr_desc')}</p>
        </div>
        <button
          onclick={onclose}
          class="text-zinc-400 hover:text-zinc-600 dark:hover:text-zinc-200 cursor-pointer p-1"
        >
          <X class="w-5 h-5" />
        </button>
      </div>

      <!-- QR Display & Live Polling Status -->
      <div class="py-2 flex flex-col items-center justify-center min-h-[260px]">
        {#if qrStatus === 'generating'}
          <div class="w-56 h-56 rounded-xl bg-zinc-100 dark:bg-zinc-800/60 flex flex-col items-center justify-center gap-3">
            <RefreshCw class="w-8 h-8 text-zinc-400 animate-spin" />
            <span class="text-xs text-zinc-500">{t('adapters.qq_qr_generating')}</span>
          </div>
        {:else if qrStatus === 'waiting' && qrCodeUrl}
          <div class="p-3 bg-white rounded-xl shadow-xs border border-zinc-200 dark:border-zinc-700">
            <img
              src={`https://api.qrserver.com/v1/create-qr-code/?size=220x220&data=${encodeURIComponent(qrCodeUrl)}`}
              alt="QQ Official Login QR Code"
              class="w-52 h-52 object-contain"
            />
          </div>
          <div class="mt-3.5 flex items-center gap-2 text-xs font-medium text-emerald-600 dark:text-emerald-400">
            <span class="relative flex h-2.5 w-2.5">
              <span class="animate-ping absolute inline-flex h-full w-full rounded-full bg-emerald-400 opacity-75"></span>
              <span class="relative inline-flex rounded-full h-2.5 w-2.5 bg-emerald-500"></span>
            </span>
            <span>{t('adapters.qq_qr_waiting')}</span>
          </div>
        {:else if qrStatus === 'success'}
          <div class="w-56 h-56 rounded-xl bg-emerald-500/10 border border-emerald-500/20 flex flex-col items-center justify-center gap-3 p-4">
            <CheckCircle2 class="w-12 h-12 text-emerald-500" />
            <div class="space-y-1">
              <span class="text-sm font-semibold text-emerald-700 dark:text-emerald-300">授权绑定成功！</span>
              <p class="text-xs font-mono text-emerald-600 dark:text-emerald-400">AppID: {qrBoundAppId}</p>
            </div>
            <p class="text-[11px] text-zinc-500">{t('adapters.qq_qr_success')}</p>
          </div>
        {:else if qrStatus === 'expired'}
          <div class="w-56 h-56 rounded-xl bg-amber-500/10 border border-amber-500/20 flex flex-col items-center justify-center gap-3 p-4">
            <AlertTriangle class="w-10 h-10 text-amber-500" />
            <span class="text-xs text-amber-700 dark:text-amber-300">{t('adapters.qq_qr_expired')}</span>
            <button
              onclick={startLogin}
              class="px-3.5 py-1.5 bg-amber-600 hover:bg-amber-500 text-white rounded-lg text-xs font-medium transition cursor-pointer"
            >
              {t('adapters.qq_qr_retry')}
            </button>
          </div>
        {:else if qrStatus === 'error'}
          <div class="w-56 h-56 rounded-xl bg-rose-500/10 border border-rose-500/20 flex flex-col items-center justify-center gap-3 p-4">
            <XCircle class="w-10 h-10 text-rose-500" />
            <span class="text-xs text-rose-700 dark:text-rose-300">{qrStatusMsg || 'Error'}</span>
            <button
              onclick={startLogin}
              class="px-3.5 py-1.5 bg-rose-600 hover:bg-rose-500 text-white rounded-lg text-xs font-medium transition cursor-pointer"
            >
              {t('common.retry')}
            </button>
          </div>
        {/if}
      </div>

      <!-- Action buttons -->
      {#if qrCodeUrl && qrStatus === 'waiting'}
        <div class="flex items-center justify-center gap-2 pt-1">
          <a
            href={qrCodeUrl}
            target="_blank"
            rel="noopener noreferrer"
            class="px-3 py-1.5 text-xs font-medium rounded-lg bg-indigo-50 dark:bg-indigo-950/40 text-indigo-600 dark:text-indigo-400 hover:bg-indigo-100 dark:hover:bg-indigo-900/60 transition flex items-center gap-1.5"
          >
            <ExternalLink class="w-3.5 h-3.5" />
            <span>{t('adapters.qq_qr_open_link')}</span>
          </a>
          <button
            onclick={copyQrUrl}
            class="px-3 py-1.5 text-xs font-medium rounded-lg border border-zinc-200 dark:border-zinc-700 text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 transition flex items-center gap-1.5 cursor-pointer"
          >
            {#if qrCopied}
              <Check class="w-3.5 h-3.5 text-emerald-500" />
              <span>{t('adapters.qq_qr_copied')}</span>
            {:else}
              <Copy class="w-3.5 h-3.5" />
              <span>{t('adapters.qq_qr_copy_link')}</span>
            {/if}
          </button>
        </div>
      {/if}

      <div class="border-t border-zinc-200 dark:border-zinc-800 pt-3 flex justify-end">
        <button
          onclick={onclose}
          class="px-4 py-2 text-xs sm:text-sm bg-zinc-900 hover:bg-zinc-800 dark:bg-zinc-100 dark:hover:bg-zinc-200 text-white dark:text-zinc-900 rounded-lg font-medium transition cursor-pointer"
        >
          {t('common.close')}
        </button>
      </div>
    </div>
  </div>
{/if}

