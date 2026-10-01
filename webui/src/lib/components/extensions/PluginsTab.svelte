<script lang="ts">
import { Plus, Puzzle, RefreshCw } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { i18n, t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { PluginHost, PluginMeta } from '../../types';
import EmptyState from '../ui/EmptyState.svelte';
import Switch from '../ui/Switch.svelte';
import InstallPluginModal from './InstallPluginModal.svelte';
import PluginConfigDrawer from './PluginConfigDrawer.svelte';

/**
 * Installed plugins, one card each, whether or not a host process runs them.
 *
 * A plugin without a host is listed too: a disabled plugin is exactly the one an operator needs
 * to find again to turn it back on. Process details (runtime, PID, host) stay on one quiet line
 * for whoever needs them.
 */

interface Row {
  plugin: PluginMeta;
  /** Host process running the plugin; `null` when it is disabled or failed to launch. */
  host: PluginHost | null;
}

let rows = $state<Row[]>([]);
let loaded = $state(false);
let loading = $state(false);
let error = $state<string | null>(null);
/** Plugins with a request in flight, so their controls cannot be pressed twice. */
let busy = $state<Record<string, boolean>>({});

let installOpen = $state(false);
let configFor = $state<string | null>(null);

async function load() {
  loading = true;
  error = null;
  try {
    const res = await api.getPlugins();
    const hosted = res.hosts.flatMap((host) =>
      host.plugins.map((plugin) => ({ plugin, host })),
    );
    const hostedIds = new Set(hosted.map((row) => row.plugin.id));
    rows = [
      ...hosted,
      ...res.plugins
        .filter((plugin) => !hostedIds.has(plugin.id))
        .map((plugin) => ({ plugin, host: null })),
    ];
  } catch (e) {
    error = errorText(e);
  } finally {
    loading = false;
    loaded = true;
  }
}

$effect(() => {
  untrack(() => void load());
});

type Tone = 'ok' | 'warn' | 'bad' | 'idle';

function statusOf(row: Row): { tone: Tone; label: string } {
  const { plugin, host } = row;
  if (!plugin.enabled)
    return { tone: 'idle', label: t('extensions.state_off') };
  if (host?.status === 'RuntimeUnavailable') {
    return { tone: 'warn', label: t('extensions.state_no_runtime') };
  }
  if (!host) return { tone: 'idle', label: t('extensions.state_not_running') };
  switch (plugin.health?.state) {
    case 'crashed':
      return { tone: 'bad', label: t('extensions.state_crashed') };
    case 'restarting':
      return { tone: 'warn', label: t('extensions.state_restarting') };
    default:
      return { tone: 'ok', label: t('extensions.state_running') };
  }
}

const CHIP: Record<Tone, string> = {
  ok: 'chip-ok',
  warn: 'chip-warn',
  bad: 'chip-bad',
  idle: 'chip-muted',
};

function listJoin(items: string[]): string {
  return items.join(i18n.locale === 'zh' ? '、' : ', ');
}

/** Enabling is harmless and immediate; disabling stops a process, so it asks first. */
async function setEnabled(plugin: PluginMeta, next: boolean) {
  if (!next) {
    const yes = await confirmDialog({
      title: t('extensions.plugin_off_title', { name: plugin.name }),
      message: t('extensions.plugin_off_text'),
      confirm: t('extensions.plugin_off_confirm'),
    });
    if (!yes) return;
  }
  busy = { ...busy, [plugin.id]: true };
  try {
    await api.setPluginEnabled(plugin.id, next);
    toasts.ok(
      t(next ? 'extensions.on_toast' : 'extensions.off_toast', {
        name: plugin.name,
      }),
    );
    await load();
  } catch (e) {
    toasts.error(
      t('extensions.toggle_failed', { name: plugin.name, error: errorText(e) }),
    );
  } finally {
    busy = { ...busy, [plugin.id]: false };
  }
}

/**
 * Restarts the host process running a plugin. The gateway addresses a host through any plugin it
 * runs, so the plugin id is what is sent, while the question names every plugin that will blink.
 */
async function restart(row: Row) {
  if (!row.host) return;
  const names = row.host.plugins.map((plugin) => plugin.name);
  const yes = await confirmDialog({
    title: t('extensions.restart_title', { name: row.plugin.name }),
    message:
      names.length > 1
        ? t('extensions.restart_text_shared', { names: listJoin(names) })
        : t('extensions.restart_text'),
    confirm: t('extensions.restart'),
  });
  if (!yes) return;
  busy = { ...busy, [row.plugin.id]: true };
  try {
    await api.restartPlugin(row.plugin.id);
    toasts.ok(t('extensions.restarted_toast', { name: row.plugin.name }));
    await load();
  } catch (e) {
    toasts.error(errorText(e));
  } finally {
    busy = { ...busy, [row.plugin.id]: false };
  }
}
</script>

<div class="flex flex-wrap items-center justify-between gap-3 px-1">
  <p class="m-0 max-w-[68ch] hint">{t('extensions.plugins_hint')}</p>
  <div class="flex flex-wrap gap-2.5">
    <button type="button" class="btn" disabled={loading} onclick={() => void load()}>
      <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
      {t('platforms.refresh')}
    </button>
    <button type="button" class="btn btn-primary" onclick={() => (installOpen = true)}>
      <Plus size={16} strokeWidth={2.2} />
      {t('extensions.install_plugin')}
    </button>
  </div>
</div>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if loaded && rows.length === 0 && !error}
  <div class="card">
    <EmptyState icon={Puzzle} title={t('extensions.plugins_empty')} text={t('extensions.plugins_empty_text')}>
      {#snippet action()}
        <button type="button" class="btn btn-primary" onclick={() => (installOpen = true)}>
          <Plus size={16} strokeWidth={2.2} />
          {t('extensions.install_plugin')}
        </button>
      {/snippet}
    </EmptyState>
  </div>
{:else if rows.length > 0}
  <div class="group-list">
    {#each rows as row (row.plugin.id)}
      {@const plugin = row.plugin}
      {@const status = statusOf(row)}
      {@const restarts = plugin.health?.restarts ?? 0}
      <article class="flex flex-wrap items-start gap-x-6 gap-y-3 py-5">
        <div class="flex min-w-0 flex-1 basis-[340px] flex-col gap-1.5">
          <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
            <h2 class="m-0 text-[17px] font-semibold">{plugin.name}</h2>
            <span class="text-[13px] text-fg3">v{plugin.version}</span>
            <span class="chip chip-sm {CHIP[status.tone]}">
              {#if status.tone !== 'idle'}<i class="dot dot-{status.tone}"></i>{/if}
              {status.label}
            </span>
          </div>
          {#if plugin.description}
            <p class="m-0 max-w-[72ch] text-[14px] text-fg2">{plugin.description}</p>
          {/if}
          {#if plugin.commands.length > 0 || plugin.tools.length > 0}
            <div class="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-[13.5px]">
              {#if plugin.commands.length > 0}
                <span class="flex flex-wrap items-center gap-1.5">
                  <span class="text-fg2">{t('extensions.commands')}</span>
                  {#each plugin.commands as command (command.name)}
                    <code class="rounded-md bg-sunk px-1.5 py-0.5 text-[12.5px]" title={command.description}>/{command.name}</code>
                  {/each}
                </span>
              {/if}
              {#if plugin.tools.length > 0}
                <span class="flex flex-wrap items-center gap-1.5">
                  <span class="text-fg2">{t('extensions.tools')}</span>
                  {#each plugin.tools as tool (tool.name)}
                    <code class="rounded-md bg-sunk px-1.5 py-0.5 text-[12.5px]" title={tool.description}>{tool.name}</code>
                  {/each}
                </span>
              {/if}
            </div>
          {/if}
          <p class="m-0 flex flex-wrap gap-x-4 text-[12.5px] text-fg3">
            <span>{plugin.id}</span>
            {#if row.host}
              {#if row.host.runtime}<span>{row.host.runtime}</span>{/if}
              {#if row.host.pid}<span>{t('extensions.pid', { pid: row.host.pid })}</span>{/if}
              {#if row.host.host_id !== plugin.id}<span>{t('extensions.host', { host: row.host.host_id })}</span>{/if}
            {/if}
            {#if restarts > 0}
              <span class="text-warn">{t('extensions.restarts', { n: restarts })}</span>
            {/if}
          </p>
          {#if status.tone === 'bad' && plugin.health?.last_error}
            <div class="notice notice-bad mt-1">
              <span class="min-w-0 break-words">{plugin.health.last_error}</span>
            </div>
          {:else if status.tone === 'warn' && row.host?.status === 'RuntimeUnavailable'}
            <div class="notice notice-warn mt-1">
              <span class="min-w-0">{t('extensions.no_runtime_hint')}</span>
            </div>
          {/if}
        </div>

        <div class="ml-auto flex items-center gap-2.5">
          {#if row.host}
            <button
              type="button"
              class="btn btn-sm btn-quiet"
              disabled={busy[plugin.id]}
              onclick={() => void restart(row)}
            >
              {t('extensions.restart')}
            </button>
          {/if}
          <button type="button" class="btn btn-sm" onclick={() => (configFor = plugin.id)}>
            {t('platforms.settings')}
          </button>
          <Switch
            checked={plugin.enabled}
            disabled={busy[plugin.id]}
            label={plugin.enabled
              ? t('platforms.turn_off', { name: plugin.name })
              : t('platforms.turn_on', { name: plugin.name })}
            onchange={(next) => void setEnabled(plugin, next)}
          />
        </div>
      </article>
    {/each}
  </div>
{:else if !error}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{/if}

<InstallPluginModal
  open={installOpen}
  onclose={() => (installOpen = false)}
  oninstalled={() => void load()}
/>

<PluginConfigDrawer
  pluginId={configFor}
  name={rows.find((row) => row.plugin.id === configFor)?.plugin.name ?? configFor ?? ''}
  onclose={() => (configFor = null)}
/>
