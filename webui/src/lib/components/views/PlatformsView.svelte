<script lang="ts">
import { Plug, RefreshCw } from 'lucide-svelte';
import { untrack } from 'svelte';
import { api } from '../../api/client';
import { errorText } from '../../format';
import { i18n, t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { milkyStore } from '../../stores/milky.svelte';
import { onebotStore } from '../../stores/onebot.svelte';
import { qqofficialStore } from '../../stores/qqofficial.svelte';
import { router } from '../../stores/router.svelte';
import { toasts } from '../../stores/toast.svelte';
import { ownersByPlatform } from '../../timeline';
import type { AdapterItem } from '../../types';
import MilkyPanel from '../platforms/MilkyPanel.svelte';
import OneBotPanel from '../platforms/OneBotPanel.svelte';
import QqOfficialPanel from '../platforms/QqOfficialPanel.svelte';
import Button from '../ui/Button.svelte';
import EmptyState from '../ui/EmptyState.svelte';
import Modal from '../ui/Modal.svelte';
import PageHead from '../ui/PageHead.svelte';
import Switch from '../ui/Switch.svelte';

/**
 * Platforms: the chat services the node talks to, whether each is connected, and which instance
 * answers on it.
 *
 * Built-in platforms can be switched on and off right here and configured in a drawer, reachable
 * as `#/platforms/<platform>` so a warning elsewhere can link straight to the right settings.
 * Platforms provided by plugins are listed for completeness; their settings live with the plugin.
 */

type Builtin = 'milky' | 'onebot' | 'qqofficial';
type BuiltinStore =
  | typeof milkyStore
  | typeof onebotStore
  | typeof qqofficialStore;

const STORES: Record<Builtin, BuiltinStore> = {
  milky: milkyStore,
  onebot: onebotStore,
  qqofficial: qqofficialStore,
};

let adapters = $state<AdapterItem[]>([]);
let loaded = $state(false);
let loading = $state(false);
let error = $state<string | null>(null);

/** Which built-in adapter owns a platform id, if any. */
function builtinOf(platform: string): Builtin | null {
  if (platform === milkyStore.platformId) return 'milky';
  if (platform === onebotStore.platformId) return 'onebot';
  if (platform === qqofficialStore.platformId) return 'qqofficial';
  return null;
}

/** Reloads the catalog and the built-in adapters' live status (never their half-typed forms). */
async function load() {
  loading = true;
  error = null;
  try {
    const res = await api.getAdapters();
    adapters = res.adapters;
    await Promise.all([
      milkyStore.ensureLoaded(),
      onebotStore.ensureLoaded(),
      qqofficialStore.ensureLoaded(),
    ]);
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

const owners = $derived(ownersByPlatform(instancesStore.instances));
const connectedCount = $derived(
  adapters.filter((adapter) => tone(adapter) === 'ok').length,
);

type Tone = 'ok' | 'warn' | 'bad' | 'idle';

/** The built-in adapter's own state is finer than "connected or not", so it wins when known. */
function tone(adapter: AdapterItem): Tone {
  const builtin = builtinOf(adapter.platform);
  if (builtin && STORES[builtin].status) return STORES[builtin].stateTone;
  return adapter.connected ? 'ok' : 'idle';
}

function stateLabel(adapter: AdapterItem): string {
  const builtin = builtinOf(adapter.platform);
  if (builtin && STORES[builtin].status)
    return t(STORES[builtin].stateLabelKey);
  return adapter.connected
    ? t('platforms.state_connected')
    : t('platforms.state_offline');
}

const CHIP: Record<Tone, string> = {
  ok: 'chip-ok',
  warn: 'chip-warn',
  bad: 'chip-bad',
  idle: 'chip-muted',
};
const DOT: Record<Tone, string> = {
  ok: 'dot-ok',
  warn: 'dot-warn',
  bad: 'dot-bad',
  idle: '',
};

/** The account the platform is logged in as, when the adapter reports one. */
function account(builtin: Builtin | null): string | null {
  switch (builtin) {
    case 'milky': {
      const login = milkyStore.status?.login;
      return login ? `${login.nickname} (${login.uin})` : null;
    }
    case 'onebot':
      return onebotStore.status?.self_id ?? null;
    case 'qqofficial':
      return qqofficialStore.status?.bot_name ?? null;
    default:
      return null;
  }
}

function listJoin(items: string[]): string {
  return items.join(i18n.locale === 'zh' ? '、' : ', ');
}

/**
 * Turns a built-in platform on or off at once, with an undo in the confirmation, the same way an
 * instance's power switch works.
 */
async function setEnabled(
  adapter: AdapterItem,
  builtin: Builtin,
  next: boolean,
) {
  const store = STORES[builtin];
  await store.setEnabled(next);
  // A failure stays on screen next to the switch (or in the open drawer) instead of in a toast
  // that disappears before it can be read.
  if (store.error) return;
  void instancesStore.refreshStatus();
  void load();
  toasts.ok(
    t(next ? 'platforms.on_toast' : 'platforms.off_toast', {
      name: adapter.display_name,
    }),
    {
      label: t('common.undo'),
      run: () => void setEnabled(adapter, builtin, !next),
    },
  );
}

// The drawer follows the address, so the browser's back button and links from other pages both
// open and close it.
const openAdapter = $derived(
  router.page === 'platforms' && router.param
    ? (adapters.find((adapter) => adapter.platform === router.param) ?? null)
    : null,
);
const openBuiltin = $derived(
  openAdapter ? builtinOf(openAdapter.platform) : null,
);
const openStore = $derived(openBuiltin ? STORES[openBuiltin] : null);

// Every time the drawer opens, the form is reloaded from the node so it never shows edits that
// were abandoned the last time it was closed.
$effect(() => {
  const store = openStore;
  if (store) untrack(() => void store.load());
});

function openSettings(platform: string) {
  router.replaceParam(platform);
}

function closeSettings() {
  router.replaceParam(null);
}

async function save() {
  const store = openStore;
  const adapter = openAdapter;
  if (!store || !adapter) return;
  await store.save();
  if (store.error) return;
  void instancesStore.refreshStatus();
  void load();
  toasts.ok(t('platforms.saved_toast', { name: adapter.display_name }));
}
</script>

<PageHead title={t('nav.platforms')}>
  {#snippet sub()}
    {#if loaded && adapters.length > 0}
      <span>
        {adapters.length === 1
          ? t('platforms.summary_one', { on: connectedCount })
          : t('platforms.summary', { total: adapters.length, on: connectedCount })}
      </span>
    {/if}
  {/snippet}
  {#snippet actions()}
    <Button type="button" disabled={loading} onclick={() => void load()}>
      <RefreshCw size={16} strokeWidth={2} class={loading ? 'animate-spin' : ''} />
      {t('platforms.refresh')}
    </Button>
  {/snippet}
</PageHead>

<p class="m-0 -mt-2 max-w-[68ch] px-1 hint">{t('platforms.intro')}</p>

{#if error}
  <div class="notice notice-bad">{error}</div>
{/if}

{#if loaded && adapters.length === 0 && !error}
  <div class="card">
    <EmptyState icon={Plug} title={t('platforms.empty_title')} text={t('platforms.empty_text')} />
  </div>
{:else if adapters.length > 0}
  <div class="group-list">
    {#each adapters as adapter (adapter.platform)}
      {@const builtin = builtinOf(adapter.platform)}
      {@const store = builtin ? STORES[builtin] : null}
      {@const level = tone(adapter)}
      {@const owner = owners.get(adapter.platform)}
      {@const who = account(builtin)}
      <article class="flex flex-wrap items-center gap-x-6 gap-y-3 py-5">
        <div class="flex min-w-0 flex-1 basis-[320px] flex-col gap-1.5">
          <div class="flex flex-wrap items-center gap-x-2.5 gap-y-1">
            <h2 class="m-0 text-[17px] font-semibold">{adapter.display_name}</h2>
            <span class="chip chip-sm {CHIP[level]}">
              {#if DOT[level]}<i class="dot {DOT[level]}"></i>{/if}
              {stateLabel(adapter)}
            </span>
          </div>
          <dl class="m-0 flex flex-wrap gap-x-5 gap-y-0.5 text-[13.5px]">
            <div class="flex gap-1.5">
              <dt class="text-fg2">{t('platforms.used_by')}</dt>
              <dd class="m-0 font-medium">
                {#if owner}
                  <a href="#/instances/{encodeURIComponent(owner.id)}" class="text-fg hover:text-accent">
                    {owner.name}
                  </a>
                {:else}
                  <span class="font-medium text-fg3">{t('platforms.unused')}</span>
                {/if}
              </dd>
            </div>
            {#if who}
              <div class="flex min-w-0 gap-1.5">
                <dt class="text-fg2">{t('platforms.account')}</dt>
                <dd class="m-0 truncate font-medium">{who}</dd>
              </div>
            {/if}
            {#if adapter.kind === 'plugin' && adapter.plugin_id}
              <div class="flex gap-1.5">
                <dt class="text-fg2">{t('platforms.plugin')}</dt>
                <dd class="m-0 font-medium"><code class="text-[12.5px]">{adapter.plugin_id}</code></dd>
              </div>
            {/if}
          </dl>
          {#if adapter.capabilities.length > 0}
            <p class="m-0 text-[13px] text-fg3">
              {t('platforms.supports', {
                list: listJoin(adapter.capabilities.map((c) => t(`capability.${c}`))),
              })}
            </p>
          {/if}
          {#if store?.error && openStore !== store}
            <div class="notice notice-bad mt-1.5">
              <span class="min-w-0 break-words">{store.error}</span>
            </div>
          {/if}
        </div>

        {#if builtin && store}
          <div class="ml-auto flex items-center gap-3">
            <Button type="button" size="sm" onclick={() => openSettings(adapter.platform)}>
              {t('platforms.settings')}
            </Button>
            <Switch
              checked={store.status?.enabled ?? false}
              disabled={store.loading || store.applyingEnabled || store.unavailable}
              label={store.status?.enabled
                ? t('platforms.turn_off', { name: adapter.display_name })
                : t('platforms.turn_on', { name: adapter.display_name })}
              onchange={(next) => void setEnabled(adapter, builtin, next)}
            />
          </div>
        {/if}
      </article>
    {/each}
  </div>
{:else if !error}
  <p class="m-0 px-1 hint">{t('common.loading')}</p>
{/if}

<Modal
  open={openStore !== null}
  title={openAdapter?.display_name ?? ''}
  variant="drawer"
  width="max-w-[560px]"
  locked={openStore?.saving ?? false}
  onclose={closeSettings}
>
  {#if openAdapter && openBuiltin && openStore}
    {@const level = tone(openAdapter)}
    {@const who = account(openBuiltin)}
    <div class="mb-6 flex flex-wrap items-center gap-x-3 gap-y-2 rounded-2xl bg-sunk py-2.5 pr-3 pl-3.5">
      <span class="chip chip-sm {CHIP[level]} {level === 'idle' ? 'bg-card' : ''}">
        {#if DOT[level]}<i class="dot {DOT[level]}"></i>{/if}
        {stateLabel(openAdapter)}
      </span>
      {#if who}<span class="min-w-0 truncate text-[13.5px] font-medium">{who}</span>{/if}
      <span class="ml-auto flex items-center gap-2.5 text-[14px] font-medium">
        {t('platforms.power')}
        <Switch
          checked={openStore.status?.enabled ?? false}
          disabled={openStore.loading || openStore.applyingEnabled || openStore.unavailable}
          label={openStore.status?.enabled
            ? t('platforms.turn_off', { name: openAdapter.display_name })
            : t('platforms.turn_on', { name: openAdapter.display_name })}
          onchange={(next) => {
            if (openAdapter && openBuiltin) void setEnabled(openAdapter, openBuiltin, next);
          }}
        />
      </span>
    </div>

    {#if openBuiltin === 'milky'}
      <MilkyPanel />
    {:else if openBuiltin === 'onebot'}
      <OneBotPanel />
    {:else}
      <QqOfficialPanel />
    {/if}

    {#if openStore.error}
      <div class="notice notice-bad mt-5">
        <span class="min-w-0 break-words">{openStore.error}</span>
      </div>
    {/if}
  {/if}

  {#snippet footer()}
    {#if openStore && !openStore.unavailable}
      {#if openBuiltin === 'milky'}
        <Button
          type="button"
         
          disabled={milkyStore.testing || milkyStore.saving}
          onclick={() => void milkyStore.test()}
        >
          {milkyStore.testing ? t('adapters.milky_testing') : t('adapters.milky_test')}
        </Button>
      {/if}
      <Button
        type="button"
        variant="filled"
        disabled={openStore.saving || openStore.loading}
        onclick={() => void save()}
      >
        {openStore.saving ? t('platforms.saving') : t('platforms.save')}
      </Button>
    {:else}
      <Button type="button" onclick={closeSettings}>{t('common.close')}</Button>
    {/if}
  {/snippet}
</Modal>
