<script lang="ts">
import { QrCode, TriangleAlert } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { qqofficialStore as store } from '../../stores/qqofficial.svelte';
import { toasts } from '../../stores/toast.svelte';
import SecretInput from '../ui/SecretInput.svelte';
import Switch from '../ui/Switch.svelte';
import QqOfficialQrModal from './QqOfficialQrModal.svelte';

/**
 * Credentials and delivery options of the built-in QQ Official adapter, shown in the platform
 * drawer.
 *
 * Scanning a QR code is offered first because it is the shortest way in: the node fills in, saves
 * and applies the AppID and AppSecret itself, so the panel only reloads afterwards. Typing them in
 * by hand stays available underneath.
 */

let qrOpen = $state(false);

async function bound() {
  await store.load();
  void instancesStore.refreshStatus();
  toasts.ok(t('adapters.qq_qr_success'));
}
</script>

{#if store.unavailable}
  <div class="notice notice-info">{t('adapters.qq_not_hosted')}</div>
{:else}
  <div class="flex flex-wrap items-center gap-x-4 gap-y-3 rounded-2xl bg-accent-tint py-3 pr-3 pl-4">
    <span class="min-w-0 flex-1 text-[14px] text-accent-fg">{t('platforms.qq_scan_hint')}</span>
    <button type="button" class="btn btn-primary btn-sm" onclick={() => (qrOpen = true)}>
      <QrCode size={16} strokeWidth={2.4} />
      {t('adapters.qq_qr_btn')}
    </button>
  </div>

  <div class="mt-5 flex flex-col gap-4">
    <div>
      <label class="label" for="qq-appid">{t('adapters.qq_appid')}</label>
      <input
        id="qq-appid"
        class="input mono"
        spellcheck="false"
        placeholder="102xxxxxx"
        bind:value={store.formAppId}
      />
    </div>
    <div>
      <label class="label" for="qq-secret">{t('adapters.qq_secret')}</label>
      <SecretInput
        id="qq-secret"
        bind:value={store.formSecret}
        placeholder={store.status?.secret_configured
          ? t('adapters.qq_secret_keep')
          : t('adapters.qq_secret_none')}
      />
    </div>
    <div class="flex items-center gap-3 text-[14.5px]">
      <span class="flex-1">{t('adapters.qq_use_markdown')}</span>
      <Switch
        checked={store.formMarkdown}
        label={t('adapters.qq_use_markdown')}
        onchange={(next) => (store.formMarkdown = next)}
      />
    </div>
    <div class="flex items-center gap-3 text-[14.5px]">
      <span class="flex-1">{t('adapters.qq_sandbox')}</span>
      <Switch
        checked={store.formSandbox}
        label={t('adapters.qq_sandbox')}
        onchange={(next) => (store.formSandbox = next)}
      />
    </div>
  </div>

  {#if store.status?.last_error}
    <div class="notice notice-warn mt-4">
      <TriangleAlert size={16} strokeWidth={2.2} class="mt-0.5 shrink-0" />
      <span class="min-w-0 break-words">{store.status.last_error}</span>
    </div>
  {/if}
{/if}

<QqOfficialQrModal open={qrOpen} onclose={() => (qrOpen = false)} onbound={() => void bound()} />
