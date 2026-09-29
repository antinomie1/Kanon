<script lang="ts">
import {
  Activity,
  Blocks,
  Cpu,
  Languages,
  MessageSquare,
  Moon,
  RefreshCw,
  Search,
  Settings,
  Sparkles,
  Sun,
  Terminal,
  Trash2,
  Users,
} from 'lucide-svelte';
import { i18n, t } from '../../stores/i18n.svelte';
import { logStore } from '../../stores/logs.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { theme } from '../../stores/theme.svelte';

let { isOpen = $bindable(false), onSelectTab = (_tab: string) => {} } = $props<{
  isOpen: boolean;
  onSelectTab?: (tab: string) => void;
}>();

let query = $state('');

const commands = $derived([
  {
    id: 'overview',
    title: `${t('nav.overview')} (Overview)`,
    category: 'Navigation',
    icon: Activity,
    action: () => onSelectTab('overview'),
  },
  {
    id: 'chat',
    title: `${t('nav.chat')} (Chat)`,
    category: 'Navigation',
    icon: MessageSquare,
    action: () => onSelectTab('chat'),
  },
  {
    id: 'pipeline',
    title: `${t('nav.pipeline')} (Pipeline & Logs)`,
    category: 'Navigation',
    icon: Terminal,
    action: () => onSelectTab('pipeline'),
  },
  {
    id: 'plugins',
    title: `${t('nav.plugins')} (Plugins & Adapters)`,
    category: 'Navigation',
    icon: Blocks,
    action: () => onSelectTab('plugins'),
  },
  {
    id: 'sessions',
    title: `${t('nav.sessions')} (Sessions)`,
    category: 'Navigation',
    icon: Users,
    action: () => onSelectTab('sessions'),
  },
  {
    id: 'personas',
    title: `${t('nav.personas')} (Personas)`,
    category: 'Navigation',
    icon: Sparkles,
    action: () => onSelectTab('personas'),
  },
  {
    id: 'providers',
    title: `${t('nav.providers')} (Model Providers)`,
    category: 'Navigation',
    icon: Cpu,
    action: () => onSelectTab('providers'),
  },
  {
    id: 'system',
    title: `${t('nav.system')} (System Settings)`,
    category: 'Navigation',
    icon: Settings,
    action: () => onSelectTab('system'),
  },
  {
    id: 'clear_logs',
    title: 'Clear WebSocket Log Buffer',
    category: 'Action',
    icon: Trash2,
    action: () => logStore.clear(),
  },
  {
    id: 'refresh',
    title: t('common.refresh'),
    category: 'Action',
    icon: RefreshCw,
    action: () => nodeStore.refresh(),
  },
  {
    id: 'toggle_lang',
    title: `Switch Language: ${i18n.locale === 'zh' ? 'English' : '简体中文'}`,
    category: 'Action',
    icon: Languages,
    action: () => i18n.toggle(),
  },
  {
    id: 'toggle_theme',
    title: 'Toggle Light / Dark Theme',
    category: 'Action',
    icon: theme.dark ? Sun : Moon,
    action: () => theme.toggle(),
  },
]);

let filtered = $derived(
  commands.filter(
    (c) =>
      c.title.toLowerCase().includes(query.toLowerCase()) ||
      c.category.toLowerCase().includes(query.toLowerCase()),
  ),
);

function execute(cmd: (typeof commands)[0]) {
  cmd.action();
  isOpen = false;
  query = '';
}

function handleKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    isOpen = false;
  }
}
</script>

{#if isOpen}
  <!-- Backdrop -->
  <!-- svelte-ignore a11y_click_events_have_key_events -->
  <div
    class="fixed inset-0 bg-black/40 backdrop-blur-xs z-50 flex items-start justify-center pt-24 px-4"
    onclick={() => (isOpen = false)}
    role="button"
    tabindex="-1"
  >
    <!-- Modal -->
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <div
      class="w-full max-w-lg bg-white dark:bg-zinc-900 border border-zinc-200 dark:border-zinc-800 rounded-xl shadow-2xl overflow-hidden flex flex-col"
      onclick={(e) => e.stopPropagation()}
      onkeydown={handleKeydown}
      role="dialog"
      tabindex="-1"
    >
      <div class="flex items-center gap-3 px-4 py-3 border-b border-zinc-200 dark:border-zinc-800">
        <Search class="w-4.5 h-4.5 text-zinc-400" />
        <!-- svelte-ignore a11y_autofocus -->
        <input
          type="text"
          bind:value={query}
          placeholder="Type a command or jump to page..."
          class="w-full bg-transparent text-sm sm:text-base text-zinc-900 dark:text-zinc-100 placeholder-zinc-400 focus:outline-hidden"
          autofocus
        />
        <kbd class="px-2 py-0.5 text-xs font-mono bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded text-zinc-400">ESC</kbd>
      </div>

      <div class="max-h-80 overflow-y-auto p-2 space-y-1">
        {#if filtered.length === 0}
          <div class="px-3 py-6 text-center text-sm text-zinc-400">No matching commands found</div>
        {:else}
          {#each filtered as item}
            {@const Icon = item.icon}
            <button
              onclick={() => execute(item)}
              class="w-full flex items-center justify-between px-3.5 py-2.5 rounded-lg text-sm text-zinc-700 dark:text-zinc-300 hover:bg-zinc-100 dark:hover:bg-zinc-800 hover:text-zinc-900 dark:hover:text-zinc-100 transition cursor-pointer text-left"
            >
              <div class="flex items-center gap-3">
                <Icon class="w-4 h-4 text-zinc-400" />
                <span class="font-medium">{item.title}</span>
              </div>
              <span class="text-xs font-mono text-zinc-400">{item.category}</span>
            </button>
          {/each}
        {/if}
      </div>
    </div>
  </div>
{/if}
