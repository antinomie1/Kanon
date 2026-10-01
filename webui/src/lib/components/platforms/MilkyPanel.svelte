<script lang="ts">
import { CircleCheck, TriangleAlert } from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { milkyStore as store } from '../../stores/milky.svelte';
import type { MilkyTransport } from '../../types';
import SecretInput from '../ui/SecretInput.svelte';
import Seg from '../ui/Seg.svelte';

/**
 * Connection settings of the built-in Milky adapter, shown in the platform drawer.
 *
 * The fields edit the store's local form; the drawer's footer saves them, and the node applies the
 * change to the running adapter without a restart. The on/off switch is not here: it lives in the
 * drawer header and in the platform list, and acts at once.
 */

function formatTime(millis: number | null): string {
  return millis ? new Date(millis).toLocaleString() : '—';
}
</script>

{#if store.unavailable}
  <div class="notice notice-info">{t('adapters.milky_not_hosted')}</div>
{:else}
  <div class="flex flex-col gap-4">
    <div>
      <label class="label" for="milky-url">{t('adapters.milky_base_url')}</label>
      <input
        id="milky-url"
        class="input mono"
        spellcheck="false"
        placeholder="http://127.0.0.1:3010"
        bind:value={store.formBaseUrl}
      />
    </div>
    <div>
      <span class="label">{t('adapters.milky_transport')}</span>
      <Seg
        label={t('adapters.milky_transport')}
        value={store.formTransport}
        onchange={(next: MilkyTransport) => (store.formTransport = next)}
        options={[
          { value: 'sse', label: t('adapters.milky_transport_sse') },
          { value: 'websocket', label: t('adapters.milky_transport_ws') },
        ]}
      />
    </div>
    <div>
      <label class="label" for="milky-token">{t('adapters.milky_token')}</label>
      <SecretInput
        id="milky-token"
        bind:value={store.formToken}
        placeholder={store.tokenConfigured
          ? t('adapters.milky_token_keep')
          : t('adapters.milky_token_none')}
      />
      {#if store.tokenConfigured}
        <label class="mt-2.5 flex items-center gap-2.5 text-[14px] text-fg2">
          <input type="checkbox" class="check" bind:checked={store.formClearToken} />
          {t('adapters.milky_clear_token')}
        </label>
      {/if}
    </div>
  </div>

  {#if store.testResult}
    <div class="notice notice-ok mt-4">
      <CircleCheck size={16} strokeWidth={2} class="mt-0.5 shrink-0" />
      <span>
        <b class="block">{t('platforms.test_ok', { ms: store.testResult.latency_ms })}</b>
        {store.testResult.login.nickname} ({store.testResult.login.uin}),
        {store.testResult.implementation.impl_name}
        {store.testResult.implementation.impl_version}
      </span>
    </div>
  {/if}

  {#if store.status}
    <h3 class="m-0 mt-7 mb-3 text-[15px] font-semibold">{t('platforms.activity')}</h3>
    <dl class="m-0 grid grid-cols-2 gap-2.5">
      <div class="tile">
        <dt class="text-[12.5px] font-medium text-fg2">{t('adapters.milky_impl')}</dt>
        <dd class="m-0 mt-0.5 truncate text-[14px] font-medium">
          {#if store.status.implementation}
            {store.status.implementation.impl_name}
            {store.status.implementation.impl_version}
          {:else}
            —
          {/if}
        </dd>
      </div>
      <div class="tile">
        <dt class="text-[12.5px] font-medium text-fg2">{t('adapters.milky_last_event')}</dt>
        <dd class="m-0 mt-0.5 truncate text-[14px] font-medium">
          {formatTime(store.status.last_event_at_unix_ms)}
        </dd>
      </div>
      <div class="tile">
        <dt class="text-[12.5px] font-medium text-fg2">{t('adapters.milky_counters')}</dt>
        <dd class="m-0 mt-0.5 text-[14px] font-medium tabular-nums">
          {store.status.messages_ingested} / {store.status.messages_delivered}
        </dd>
      </div>
      <div class="tile">
        <dt class="text-[12.5px] font-medium text-fg2">{t('adapters.milky_events')}</dt>
        <dd class="m-0 mt-0.5 text-[14px] font-medium tabular-nums">
          {store.status.events_received}
          {#if store.status.messages_rejected > 0}
            <span class="font-medium text-warn">
              {t('platforms.rejected', { n: store.status.messages_rejected })}
            </span>
          {/if}
        </dd>
      </div>
    </dl>
    {#if store.status.last_error}
      <div class="notice notice-warn mt-3">
        <TriangleAlert size={16} strokeWidth={2} class="mt-0.5 shrink-0" />
        <span class="min-w-0 break-words">{store.status.last_error}</span>
      </div>
    {/if}
  {/if}

  {#if store.config}
    <p class="m-0 mt-6 hint">
      {t('platforms.identity', {
        platform: store.config.platform,
        name: store.config.display_name ?? store.status?.display_name ?? store.config.platform,
      })}
    </p>
  {/if}
{/if}
