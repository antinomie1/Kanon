<script lang="ts">
import { Copy } from 'lucide-svelte';
import { api } from '../../api/client';
import { errorText, formatBytes, formatDuration } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { providersStore } from '../../stores/providers.svelte';
import { toasts } from '../../stores/toast.svelte';
import Section from '../ui/Section.svelte';

let metrics = $state<string | null>(null);
let metricsError = $state<string | null>(null);
let metricsOpen = $state(false);

$effect(() => {
  if (!providersStore.systemConfig) void providersStore.load();
});

async function loadMetrics() {
  metricsError = null;
  try {
    metrics = await api.getMetrics();
  } catch (e) {
    metricsError = errorText(e);
  }
}

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
    toasts.ok(t('settings.copied'));
  } catch (e) {
    toasts.error(errorText(e));
  }
}

const health = $derived(nodeStore.health);
const config = $derived(providersStore.systemConfig);
</script>

<Section title={t('settings.node_status')} hint={t('settings.node_status_hint')}>
  {#if health}
    <dl class="m-0 grid grid-cols-[repeat(auto-fill,minmax(170px,1fr))] gap-3">
      {#each [
        [t('settings.node_version'), health.version],
        [t('settings.node_uptime'), formatDuration(health.uptime_seconds)],
        [t('settings.node_memory'), formatBytes(health.memory.resident_bytes)],
        [t('settings.node_plugins'), t('settings.node_plugins_value', { hosts: health.plugins.hosts, loaded: health.plugins.loaded })],
        [t('settings.node_sessions'), t('settings.node_sessions_value', { total: health.sessions.total, active: health.sessions.active })],
        [t('settings.node_sockets'), String(health.realtime.websocket_connections)],
      ] as [label, value] (label)}
        <div class="tile">
          <dt class="text-[12.5px] font-bold text-fg2">{label}</dt>
          <dd class="m-0 mt-0.5 truncate text-[16px] font-extrabold tabular-nums">{value}</dd>
        </div>
      {/each}
    </dl>
  {:else}
    <p class="m-0 hint">{nodeStore.error ?? t('common.loading')}</p>
  {/if}
</Section>

<Section title={t('settings.node_paths')} hint={t('settings.node_paths_hint')}>
  {#if config}
    <div class="flex flex-col gap-2">
      {#each [
        [t('providers.ipc_socket'), config.ipc_socket_path],
        [t('providers.run_dir'), config.run_dir],
        [t('providers.data_dir'), config.data_dir],
        [t('providers.os_arch'), `${config.environment.os} (${config.environment.arch})`],
      ] as [label, value] (label)}
        <div class="flex items-center gap-3 rounded-xl bg-sunk py-1.5 pr-1.5 pl-3.5">
          <span class="w-28 shrink-0 text-[13px] font-bold text-fg2">{label}</span>
          <code class="min-w-0 flex-1 truncate text-[13px]" title={value}>{value}</code>
          <button
            type="button"
            class="btn btn-quiet btn-icon btn-xs"
            aria-label={t('settings.copy_value', { label })}
            onclick={() => copy(value)}
          >
            <Copy size={14} strokeWidth={2.2} />
          </button>
        </div>
      {/each}
    </div>
  {:else}
    <p class="m-0 hint">{providersStore.error ?? t('common.loading')}</p>
  {/if}
</Section>

<Section title={t('settings.node_metrics')} hint={t('settings.node_metrics_hint')}>
  <div>
    <button
      type="button"
      class="btn btn-sm"
      aria-expanded={metricsOpen}
      onclick={() => {
        metricsOpen = !metricsOpen;
        if (metricsOpen) void loadMetrics();
      }}
    >
      {metricsOpen ? t('settings.metrics_hide') : t('settings.metrics_show')}
    </button>
  </div>
  {#if metricsOpen}
    {#if metricsError}
      <div class="notice notice-bad">{metricsError}</div>
    {:else}
      <pre
        class="scroll-thin m-0 max-h-[420px] overflow-auto rounded-xl bg-sunk p-4 text-[12px] leading-relaxed">{metrics ??
          t('common.loading')}</pre>
    {/if}
  {/if}
</Section>
