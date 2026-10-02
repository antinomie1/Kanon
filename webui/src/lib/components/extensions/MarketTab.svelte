<script lang="ts">
import { ExternalLink, RefreshCw, Store } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { confirmDialog } from '../../stores/confirm.svelte';
import { t } from '../../stores/i18n.svelte';
import { toasts } from '../../stores/toast.svelte';
import type { MarketPlugin, MarketResponse } from '../../types';
import Button from '../ui/Button.svelte';
import EmptyState from '../ui/EmptyState.svelte';

/**
 * Plugins offered by the market indexes the operator listed in `data/system.json`.
 *
 * The node reads the indexes on every visit, so this tab only shows and installs: installing sends
 * the entry's package URL (or, without one, its repository) to the same installer the install
 * dialog uses, which checks the manifest and `kanon_version` again. An update replaces the
 * installed copy, so it asks first.
 */

let market = $state<MarketResponse | null>(null);
let loading = $state(false);
let error = $state<string | null>(null);
/** Entries with an install in flight. */
let busy = $state<Record<string, boolean>>({});

async function load() {
  loading = true;
  error = null;
  try {
    market = await api.getPluginMarket();
  } catch (e) {
    error = errorText(e);
  } finally {
    loading = false;
  }
}

$effect(() => {
  untrack(() => void load());
});

type Action = 'install' | 'update' | 'installed';

function actionOf(plugin: MarketPlugin): Action {
  if (!plugin.installed_version) return 'install';
  return plugin.installed_version === plugin.version ? 'installed' : 'update';
}

async function install(plugin: MarketPlugin) {
  const replace = actionOf(plugin) !== 'install';
  if (replace) {
    const yes = await confirmDialog({
      title: t('extensions.market_update_title', { name: plugin.name }),
      message: t('extensions.market_update_text', {
        from: plugin.installed_version ?? '',
        to: plugin.version,
      }),
      confirm: t('extensions.market_update'),
    });
    if (!yes) return;
  }
  busy = { ...busy, [plugin.id]: true };
  try {
    const res = await api.installPlugin(
      plugin.download_url
        ? { url: plugin.download_url, replace }
        : { git: plugin.repository, replace },
    );
    if (res.status === 'RuntimeUnavailable') {
      toasts.error(
        t('extensions.installed_no_runtime', {
          name: plugin.name,
          reason: res.message ?? '',
        }),
      );
    } else {
      toasts.ok(
        t(
          replace ? 'extensions.replaced_toast' : 'extensions.installed_toast',
          {
            name: plugin.name,
          },
        ),
      );
    }
    await load();
  } catch (e) {
    toasts.error(errorText(e));
  } finally {
    busy = { ...busy, [plugin.id]: false };
  }
}

const troubled = $derived(
  (market?.sources ?? []).filter(
    (source) => source.error || source.warnings.length > 0,
  ),
);
</script>

<div class="flex flex-wrap items-center justify-between gap-3 px-1">
  <p class="m-0 max-w-[68ch] hint">{t('extensions.market_hint')}</p>
  <Button type="button" disabled={loading} onclick={() => void load()}>
    <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
    {t('platforms.refresh')}
  </Button>
</div>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if market && !market.configured}
  <div class="card">
    <EmptyState icon={Store} title={t('extensions.market_unconfigured')} text={t('extensions.market_unconfigured_text')} />
    <pre class="scroll-thin m-0 mx-auto mb-6 max-w-[520px] overflow-x-auto rounded-xl bg-sunk p-3.5 text-[12.5px]">{`"plugin_market": {
  "indexes": ["https://example.org/kanon/index.json"]
}`}</pre>
  </div>
{:else if market}
  {#each troubled as source (source.url)}
    <div class="notice {source.error ? 'notice-bad' : 'notice-warn'}">
      <span class="min-w-0 break-words">
        <strong>{source.name ?? source.url}</strong>
        {#if source.error}
          — {source.error}
        {:else}
          {#each source.warnings as warning, index (index)}<br />{warning}{/each}
        {/if}
      </span>
    </div>
  {/each}

  {#if market.plugins.length === 0}
    <div class="card">
      <EmptyState icon={Store} title={t('extensions.market_empty')} />
    </div>
  {:else}
    <div class="group-list">
      {#each market.plugins as plugin (plugin.id)}
        {@const action = actionOf(plugin)}
        <article class="flex flex-wrap items-start gap-x-6 gap-y-3 py-5">
          <div class="flex min-w-0 flex-1 basis-[340px] flex-col gap-1.5">
            <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
              <h2 class="m-0 text-[17px] font-semibold">{plugin.name}</h2>
              <span class="text-[13px] text-fg3">v{plugin.version}</span>
              {#if action === 'installed'}
                <span class="chip chip-sm chip-ok">{t('extensions.market_installed')}</span>
              {:else if action === 'update'}
                <span class="chip chip-sm chip-accent">
                  {t('extensions.market_installed_version', { version: plugin.installed_version ?? '' })}
                </span>
              {/if}
              {#if !plugin.compatible}
                <span class="chip chip-sm chip-warn" title={plugin.incompatible_reason}>
                  {t('extensions.market_incompatible')}
                </span>
              {/if}
            </div>
            {#if plugin.description}
              <p class="m-0 max-w-[72ch] text-[14px] text-fg2">{plugin.description}</p>
            {/if}
            {#if plugin.platforms.length > 0}
              <div class="flex flex-wrap items-center gap-1.5 text-[13px]">
                <span class="text-fg2">{t('extensions.platforms')}</span>
                {#each plugin.platforms as platform (platform)}
                  <span class="chip chip-sm chip-muted">{platform}</span>
                {/each}
              </div>
            {/if}
            <p class="m-0 flex flex-wrap gap-x-4 text-[12.5px] text-fg3">
              <span>{plugin.id}</span>
              {#if plugin.author}<span>{plugin.author}</span>{/if}
              {#if plugin.kanon_version}<span>Kanon {plugin.kanon_version}</span>{/if}
              <span>{t('extensions.market_from', { source: plugin.source })}</span>
              {#if plugin.homepage || plugin.repository}
                <a
                  class="inline-flex items-center gap-1 text-fg3 hover:text-fg"
                  href={plugin.homepage ?? plugin.repository}
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  <ExternalLink size={12} strokeWidth={2} />
                  {t('extensions.homepage')}
                </a>
              {/if}
            </p>
            {#if !plugin.compatible && plugin.incompatible_reason}
              <div class="notice notice-warn mt-1">
                <span class="min-w-0 break-words">{plugin.incompatible_reason}</span>
              </div>
            {/if}
          </div>

          <div class="ml-auto flex items-center gap-2.5">
            <Button
              type="button"
              size="sm"
              variant={action === 'install' ? 'filled' : 'tonal'}
              disabled={busy[plugin.id] || action === 'installed' || !plugin.compatible}
              onclick={() => void install(plugin)}
            >
              {busy[plugin.id]
                ? t('extensions.installing')
                : action === 'update'
                  ? t('extensions.market_update')
                  : action === 'installed'
                    ? t('extensions.market_installed')
                    : t('extensions.install')}
            </Button>
          </div>
        </article>
      {/each}
    </div>
  {/if}
{:else if !error}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{/if}
