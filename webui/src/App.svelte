<script lang="ts">
import { Menu, RefreshCw, WifiOff } from 'lucide-svelte';
import CommandPalette from './lib/components/layout/CommandPalette.svelte';
import Sidebar from './lib/components/layout/Sidebar.svelte';
import Button from './lib/components/ui/Button.svelte';
import ConfirmHost from './lib/components/ui/ConfirmHost.svelte';
import ToastHost from './lib/components/ui/ToastHost.svelte';
import ActivityView from './lib/components/views/ActivityView.svelte';
import ExtensionsView from './lib/components/views/ExtensionsView.svelte';
import HomeView from './lib/components/views/HomeView.svelte';
import InstancesView from './lib/components/views/InstancesView.svelte';
import ModelsPage from './lib/components/views/ModelsPage.svelte';
import PersonasView from './lib/components/views/PersonasView.svelte';
import PlatformsView from './lib/components/views/PlatformsView.svelte';
import PlaygroundView from './lib/components/views/PlaygroundView.svelte';
import SessionsView from './lib/components/views/SessionsView.svelte';
import SettingsView from './lib/components/views/SettingsView.svelte';
import { t } from './lib/stores/i18n.svelte';
import { agentsStore } from './lib/stores/agents.svelte';
import { instancesStore } from './lib/stores/instances.svelte';
import { modelsStore } from './lib/stores/models.svelte';
import { nodeStore } from './lib/stores/node.svelte';
import { router } from './lib/stores/router.svelte';

let isCommandOpen = $state(false);
let isDrawerOpen = $state(false);

// Pages that fill the window themselves (a chat transcript, a live log) instead of scrolling.
const fullHeight = $derived(
  router.page === 'chat' || router.page === 'activity',
);

// The instance catalog feeds the navigation badge and the home page on every screen, so it is
// loaded once here and its connection state is re-polled while the tab is visible.
$effect(() => {
  void instancesStore.load();
  void agentsStore.load();
  void modelsStore.load();
  const timer = window.setInterval(() => {
    if (document.visibilityState === 'visible') {
      void instancesStore.refreshStatus();
    }
  }, 10000);
  return () => window.clearInterval(timer);
});

// Each page starts at the top, and the mobile drawer closes once a destination is chosen.
let scroller = $state<HTMLElement>();
$effect(() => {
  void router.page;
  scroller?.scrollTo({ top: 0 });
  isDrawerOpen = false;
});

function handleKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault();
    isCommandOpen = !isCommandOpen;
  }
}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="flex h-dvh w-full overflow-hidden bg-rail text-fg">
  <aside class="hidden w-[256px] shrink-0 rail lg:block">
    <Sidebar onOpenCommand={() => (isCommandOpen = true)} />
  </aside>

  <div class="flex min-w-0 flex-1 flex-col lg:py-3 lg:pr-3">
    <div class="flex h-14 shrink-0 items-center gap-3 rail px-4 lg:hidden">
      <Button
        type="button"
        variant="text" size="sm" square class="-ml-2"
        aria-label={t('shell.open_menu')}
        onclick={() => (isDrawerOpen = true)}
      >
        <Menu size={20} strokeWidth={2} />
      </Button>
      <span class="shrink-0 text-[20px] font-bold">Kanon Console</span>
      <span class="truncate text-[14px] text-fg2">{t(`nav.${router.page}`)}</span>
    </div>

    <main
      bind:this={scroller}
      class="scroll-thin min-h-0 flex-1 overflow-y-auto rounded-t-[28px] bg-page lg:rounded-[32px]"
    >
      <div
        class="mx-auto flex w-full max-w-[1320px] flex-col gap-5 px-4 py-5 sm:px-8 lg:px-10 lg:py-[30px] {fullHeight
          ? 'h-full'
          : ''}"
      >
        {#if nodeStore.error}
          <div class="notice notice-bad items-center" role="alert">
            <WifiOff size={18} strokeWidth={2} class="shrink-0" />
            <span class="min-w-0 flex-1">
              <b class="font-semibold">{t('shell.offline_title')}</b>
              <span class="ml-1">{t('shell.offline_text', { error: nodeStore.error })}</span>
            </span>
            <Button
              type="button"
              variant="outlined"
              size="sm"
              class="kanon-danger"
              onclick={() => nodeStore.refresh()}
            >
              <RefreshCw size={15} strokeWidth={2} />
              {t('common.retry')}
            </Button>
          </div>
        {/if}

        {#if router.page === 'home'}
          <HomeView />
        {:else if router.page === 'chat'}
          <PlaygroundView />
        {:else if router.page === 'instances'}
          <InstancesView />
        {:else if router.page === 'sessions'}
          <SessionsView />
        {:else if router.page === 'personas'}
          <PersonasView />
        {:else if router.page === 'platforms'}
          <PlatformsView />
        {:else if router.page === 'models'}
          <ModelsPage />
        {:else if router.page === 'extensions'}
          <ExtensionsView />
        {:else if router.page === 'activity'}
          <ActivityView />
        {:else if router.page === 'settings'}
          <SettingsView />
        {/if}
      </div>
    </main>
  </div>

  {#if isDrawerOpen}
    <div class="fixed inset-0 z-40 lg:hidden">
      <button
        type="button"
        class="absolute inset-0 cursor-default bg-black/32"
        aria-label={t('common.close')}
        onclick={() => (isDrawerOpen = false)}
      ></button>
      <aside class="absolute inset-y-0 left-0 w-[284px] max-w-[85vw] rounded-r-2xl rail shadow-[var(--k-pop)]">
        <Sidebar
          onOpenCommand={() => {
            isDrawerOpen = false;
            isCommandOpen = true;
          }}
          onNavigate={() => (isDrawerOpen = false)}
        />
      </aside>
    </div>
  {/if}

  <CommandPalette bind:isOpen={isCommandOpen} />
  <ToastHost />
  <ConfirmHost />
</div>
