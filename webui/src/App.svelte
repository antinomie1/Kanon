<script lang="ts">
import CommandPalette from './lib/components/layout/CommandPalette.svelte';
import Header from './lib/components/layout/Header.svelte';
import Sidebar from './lib/components/layout/Sidebar.svelte';

import InstancesView from './lib/components/views/InstancesView.svelte';
import OverviewView from './lib/components/views/OverviewView.svelte';
import PipelineLogsView from './lib/components/views/PipelineLogsView.svelte';
import PlaygroundView from './lib/components/views/PlaygroundView.svelte';
import PluginsAdaptersView from './lib/components/views/PluginsAdaptersView.svelte';
import ProvidersView from './lib/components/views/ProvidersView.svelte';
import SessionsPersonasView from './lib/components/views/SessionsPersonasView.svelte';
import SystemConfigView from './lib/components/views/SystemConfigView.svelte';

import { t } from './lib/stores/i18n.svelte';
import { nodeStore } from './lib/stores/node.svelte';

let currentTab = $state('overview');
let isCommandOpen = $state(false);

function handleKeydown(e: KeyboardEvent) {
  if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault();
    isCommandOpen = !isCommandOpen;
  }
}
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="h-screen w-screen flex bg-white dark:bg-zinc-950 text-zinc-900 dark:text-zinc-100 font-sans antialiased overflow-hidden">
  <!-- Left Navigation Sidebar -->
  <Sidebar
    {currentTab}
    onTabChange={(tab) => (currentTab = tab)}
    onOpenCommand={() => (isCommandOpen = true)}
  />

  <!-- Main Content Area -->
  <main class="flex-1 flex flex-col h-full min-w-0 overflow-hidden">
    <!-- Top Header -->
    <Header
      title={t(`title.${currentTab}`)}
      subtitle={t(`subtitle.${currentTab}`)}
    />

    <!-- Offline Notification Banner if node unreachable -->
    {#if nodeStore.error}
      <div class="px-6 py-2 bg-rose-500/10 border-b border-rose-500/20 text-rose-600 dark:text-rose-400 text-xs flex items-center justify-between font-mono">
        <span>Node unreachable: {nodeStore.error} (retrying every 5s)</span>
        <button
          onclick={() => nodeStore.refresh()}
          class="underline hover:text-rose-700 dark:hover:text-rose-300 cursor-pointer"
        >
          {t('common.retry')}
        </button>
      </div>
    {/if}

    <!-- Tab View Container -->
    <div class="flex-1 overflow-y-auto">
      {#if currentTab === 'overview'}
        <OverviewView onNavigate={(tab) => (currentTab = tab)} />
      {:else if currentTab === 'instances'}
        <InstancesView />
      {:else if currentTab === 'chat' || currentTab === 'playground'}
        <PlaygroundView />
      {:else if currentTab === 'pipeline'}
        <PipelineLogsView />
      {:else if currentTab === 'plugins'}
        <PluginsAdaptersView />
      {:else if currentTab === 'sessions'}
        <SessionsPersonasView />
      {:else if currentTab === 'providers'}
        <ProvidersView />
      {:else if currentTab === 'system'}
        <SystemConfigView />
      {/if}
    </div>
  </main>

  <!-- Global Command Palette Modal -->
  <CommandPalette
    bind:isOpen={isCommandOpen}
    onSelectTab={(tab) => (currentTab = tab)}
  />
</div>
