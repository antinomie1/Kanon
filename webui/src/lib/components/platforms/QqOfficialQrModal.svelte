<script lang="ts">
import {
  Check,
  CircleCheck,
  CircleX,
  Copy,
  ExternalLink,
  LoaderCircle,
  TriangleAlert,
} from 'lucide-svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { t } from '../../stores/i18n.svelte';
import Modal from '../ui/Modal.svelte';

/**
 * QR-code binding flow for the QQ Official adapter.
 *
 * The dialog is a self-contained flow: it requests a login task and polls it until the operator
 * authorizes (or it expires). On success the node has already applied and saved the credentials,
 * so the caller is only told the bound AppID and reloads its view.
 */
let {
  open = false,
  onclose,
  onbound,
}: {
  open?: boolean;
  onclose: () => void;
  /** Called after a successful binding, once the node runs with the new credentials. */
  onbound?: (appid: string | null) => void;
} = $props();

let qrTaskId = $state<string | null>(null);
let qrBindKey = $state<string | null>(null);
let qrCodeUrl = $state<string | null>(null);
let qrStatus = $state<
  'idle' | 'generating' | 'waiting' | 'success' | 'expired' | 'error'
>('idle');
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
          onbound?.(pollRes.appid ?? null);
        } else if (pollRes.status === 'expired') {
          stopQrPolling();
          qrStatus = 'expired';
        }
      } catch (e) {
        // A failed poll (binding service error, undecryptable secret, save failure) is shown with
        // the retry button rather than retried silently forever.
        stopQrPolling();
        qrStatus = 'error';
        qrStatusMsg = errorText(e);
      }
    }, intervalMs);
  } catch (e) {
    qrStatus = 'error';
    qrStatusMsg = errorText(e);
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
  } catch (e) {
    qrStatusMsg = errorText(e);
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

<Modal {open} {onclose} title={t('adapters.qq_qr_title')} width="max-w-md">
  <p class="m-0 hint">{t('adapters.qq_qr_desc')}</p>

  <div class="mt-5 flex min-h-[260px] flex-col items-center justify-center text-center">
    {#if qrStatus === 'generating' || qrStatus === 'idle'}
      <div class="grid h-56 w-56 place-items-center rounded-2xl bg-sunk">
        <span class="flex flex-col items-center gap-3 text-[13.5px] text-fg2">
          <LoaderCircle size={28} strokeWidth={2} class="animate-spin" />
          {t('adapters.qq_qr_generating')}
        </span>
      </div>
    {:else if qrStatus === 'waiting' && qrCodeUrl}
      <div class="rounded-2xl bg-white p-3 shadow-[inset_0_0_0_1px_var(--k-line)]">
        <img
          src={`https://api.qrserver.com/v1/create-qr-code/?size=220x220&data=${encodeURIComponent(qrCodeUrl)}`}
          alt={t('adapters.qq_qr_title')}
          class="h-52 w-52 object-contain"
        />
      </div>
      <p class="m-0 mt-3.5 flex items-center gap-2 text-[13.5px] font-medium text-ok">
        <i class="dot dot-ok animate-pulse"></i>
        {t('adapters.qq_qr_waiting')}
      </p>
    {:else if qrStatus === 'success'}
      <div class="flex h-56 w-56 flex-col items-center justify-center gap-2 rounded-2xl bg-ok-tint p-4 text-ok-fg">
        <CircleCheck size={44} strokeWidth={2} />
        <b class="text-[15px]">{t('adapters.qq_qr_done')}</b>
        <code class="text-[12.5px]">AppID {qrBoundAppId}</code>
      </div>
      <p class="m-0 mt-3.5 hint">{t('adapters.qq_qr_success')}</p>
    {:else if qrStatus === 'expired'}
      <div class="flex h-56 w-56 flex-col items-center justify-center gap-3 rounded-2xl bg-warn-tint p-4 text-warn-fg">
        <TriangleAlert size={36} strokeWidth={2} class="text-warn" />
        <span class="text-[13.5px]">{t('adapters.qq_qr_expired')}</span>
        <button
          type="button"
          class="btn btn-sm text-warn-fg shadow-[inset_0_0_0_1px_currentColor]"
          onclick={startLogin}
        >
          {t('adapters.qq_qr_retry')}
        </button>
      </div>
    {:else if qrStatus === 'error'}
      <div class="flex h-56 w-56 flex-col items-center justify-center gap-3 rounded-2xl bg-danger-tint p-4 text-danger-fg">
        <CircleX size={36} strokeWidth={2} />
        <span class="line-clamp-4 text-[13px] break-words">{qrStatusMsg ?? t('common.error')}</span>
        <button
          type="button"
          class="btn btn-sm text-danger-fg shadow-[inset_0_0_0_1px_currentColor]"
          onclick={startLogin}
        >
          {t('adapters.qq_qr_retry')}
        </button>
      </div>
    {/if}
  </div>

  {#if qrCodeUrl && qrStatus === 'waiting'}
    <div class="mt-4 flex flex-wrap items-center justify-center gap-2">
      <a href={qrCodeUrl} target="_blank" rel="noopener noreferrer" class="btn btn-sm no-underline">
        <ExternalLink size={15} strokeWidth={2} />
        {t('adapters.qq_qr_open_link')}
      </a>
      <button type="button" class="btn btn-sm" onclick={copyQrUrl}>
        {#if qrCopied}
          <Check size={15} strokeWidth={2.2} class="text-ok" />
          {t('adapters.qq_qr_copied')}
        {:else}
          <Copy size={15} strokeWidth={2} />
          {t('adapters.qq_qr_copy_link')}
        {/if}
      </button>
    </div>
  {/if}

  {#snippet footer()}
    <button type="button" class="btn {qrStatus === 'success' ? 'btn-primary' : ''}" onclick={onclose}>
      {qrStatus === 'success' ? t('platforms.done') : t('common.close')}
    </button>
  {/snippet}
</Modal>
