<script lang="ts">
import { TriangleAlert } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { onebotStore as store } from '../../stores/onebot.svelte';
import type { OneBotTransport } from '../../types';
import Checkbox from '../ui/Checkbox.svelte';
import SecretInput from '../ui/SecretInput.svelte';
import Seg from '../ui/Seg.svelte';
import TextField from '../ui/TextField.svelte';

/**
 * Connection settings of the built-in OneBot v11 adapter, shown in the platform drawer.
 *
 * The connection direction comes first because it decides what the address below means: the
 * protocol server Kanon dials (forward) or the address Kanon listens on (reverse).
 */
</script>

{#if store.unavailable}
  <div class="notice notice-info">{t('adapters.onebot_not_hosted')}</div>
{:else}
  <div class="flex flex-col gap-4">
    <div>
      <span class="label">{t('adapters.onebot_transport')}</span>
      <Seg
        label={t('adapters.onebot_transport')}
        value={store.formTransport}
        onchange={(next: OneBotTransport) => (store.formTransport = next)}
        options={[
          { value: 'forward_websocket', label: t('adapters.onebot_transport_forward') },
          { value: 'reverse_websocket', label: t('adapters.onebot_transport_reverse') },
        ]}
      />
      <p class="m-0 mt-2 hint">
        {store.formTransport === 'reverse_websocket'
          ? t('adapters.onebot_reverse_hint')
          : t('adapters.onebot_forward_hint')}
      </p>
    </div>
    <div>
      <label class="label" for="onebot-url">{t('adapters.onebot_ws_url')}</label>
      <TextField
        id="onebot-url"
        mono
        spellcheck="false"
        placeholder="ws://127.0.0.1:6700"
        bind:value={store.formWsUrl}
      />
    </div>
    <div>
      <label class="label" for="onebot-token">{t('adapters.onebot_token')}</label>
      <SecretInput
        id="onebot-token"
        bind:value={store.formToken}
        placeholder={store.tokenConfigured
          ? t('adapters.onebot_token_keep')
          : t('adapters.onebot_token_none')}
      />
      {#if store.tokenConfigured}
        <label class="mt-2.5 flex items-center gap-2.5 text-[14px] text-fg2">
          <Checkbox bind:checked={store.formClearToken} label={t('adapters.onebot_clear_token')} />
          {t('adapters.onebot_clear_token')}
        </label>
      {/if}
    </div>
  </div>

  {#if store.status?.last_error}
    <div class="notice notice-warn mt-4">
      <TriangleAlert size={16} strokeWidth={2} class="mt-0.5 shrink-0" />
      <span class="min-w-0 break-words">{store.status.last_error}</span>
    </div>
  {/if}

  {#if store.config}
    <p class="m-0 mt-6 hint">
      {t('platforms.identity', {
        platform: store.config.platform,
        name: store.config.display_name ?? store.config.platform,
      })}
    </p>
  {/if}
{/if}
