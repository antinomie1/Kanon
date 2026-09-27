<script lang="ts">
import {
  Activity,
  Blocks,
  Bot,
  Cpu,
  Languages,
  Laptop,
  MessageSquare,
  Moon,
  Radio,
  Settings,
  Sun,
  Terminal,
  Users,
} from 'lucide-svelte';
import { i18n, t } from '../../stores/i18n.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { theme } from '../../stores/theme.svelte';

let {
  currentTab = 'overview',
  onTabChange = (_tab: string) => {},
  onOpenCommand = () => {},
} = $props<{
  currentTab?: string;
  onTabChange?: (tab: string) => void;
  onOpenCommand?: () => void;
}>();

const navItems = [
  { id: 'overview', key: 'nav.overview', icon: Activity },
  { id: 'instances', key: 'nav.instances', icon: Bot },
  { id: 'chat', key: 'nav.chat', icon: MessageSquare },
  { id: 'pipeline', key: 'nav.pipeline', icon: Terminal },
  { id: 'plugins', key: 'nav.plugins', icon: Blocks },
  { id: 'sessions', key: 'nav.sessions', icon: Users },
  { id: 'providers', key: 'nav.providers', icon: Cpu },
  { id: 'system', key: 'nav.system', icon: Settings },
];
</script>

<aside class="w-64 border-r border-zinc-200 dark:border-zinc-800 bg-zinc-50/60 dark:bg-zinc-950 flex flex-col justify-between shrink-0 select-none">
  <!-- Brand Header -->
  <div class="p-4 border-b border-zinc-200/80 dark:border-zinc-800/80">
    <div class="flex items-center justify-between">
      <h1 class="text-lg font-bold tracking-tight text-zinc-900 dark:text-zinc-100">Kanon Console</h1>
      <span class="inline-flex items-center px-2 py-0.5 rounded text-xs font-medium font-mono uppercase tracking-wider
        {nodeStore.health?.status === 'ok' || nodeStore.health?.status === 'healthy' ? 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400 border border-emerald-500/20' : 'bg-rose-500/10 text-rose-600 dark:text-rose-400 border border-rose-500/20'}">
        <span class="w-1.5 h-1.5 rounded-full mr-1.5 {nodeStore.health?.status === 'ok' || nodeStore.health?.status === 'healthy' ? 'bg-emerald-500 animate-pulse' : 'bg-rose-500'}"></span>
        {nodeStore.health?.status ? (t(`status.${nodeStore.health.status}`) !== `status.${nodeStore.health.status}` ? t(`status.${nodeStore.health.status}`) : nodeStore.health.status) : t('status.connecting')}
      </span>
    </div>

    <!-- Quick search hotkey hint -->
    <button
      onclick={onOpenCommand}
      class="mt-3 w-full flex items-center justify-between px-3 py-2 text-sm text-zinc-500 dark:text-zinc-400 bg-white dark:bg-zinc-900/80 border border-zinc-200 dark:border-zinc-800 rounded-lg hover:border-zinc-300 dark:hover:border-zinc-700 transition shadow-2xs cursor-pointer"
    >
      <span class="flex items-center gap-2">
        <Radio class="w-4 h-4 text-zinc-400" />
        <span>{t('common.command_menu')}</span>
      </span>
      <kbd class="px-2 py-0.5 text-xs font-mono bg-zinc-100 dark:bg-zinc-800 border border-zinc-200 dark:border-zinc-700 rounded text-zinc-500">⌘K</kbd>
    </button>
  </div>

  <!-- Navigation items -->
  <nav class="flex-1 p-3 space-y-1.5 overflow-y-auto">
    {#each navItems as item}
      {@const Icon = item.icon}
      <button
        onclick={() => onTabChange(item.id)}
        class="w-full flex items-center gap-3 px-3.5 py-2.5 rounded-lg text-sm font-medium transition cursor-pointer
          {currentTab === item.id
            ? 'bg-zinc-900 text-white dark:bg-zinc-100 dark:text-zinc-950 font-semibold shadow-xs'
            : 'text-zinc-600 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 hover:bg-zinc-200/50 dark:hover:bg-zinc-900/60'}"
      >
        <Icon class="w-4.5 h-4.5 shrink-0" />
        <span>{t(item.key)}</span>
      </button>
    {/each}
  </nav>

  <!-- Bottom toolbar: language & theme -->
  <div class="p-3 border-t border-zinc-200/80 dark:border-zinc-800/80 space-y-2.5">
    <!-- Language toggle -->
    <div class="flex items-center justify-between px-1">
      <span class="text-xs text-zinc-500 font-mono flex items-center gap-1.5">
        <Languages class="w-3.5 h-3.5 text-zinc-400" />
        <span>{t('common.language')}</span>
      </span>
      <div class="flex items-center gap-1 bg-zinc-200/60 dark:bg-zinc-900 p-0.5 rounded-md border border-zinc-200 dark:border-zinc-800">
        <button
          onclick={() => i18n.setLocale('zh')}
          class="px-2 py-0.5 rounded text-xs font-mono transition cursor-pointer {i18n.locale === 'zh' ? 'bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 shadow-2xs font-bold' : 'text-zinc-400 hover:text-zinc-600'}"
          title="简体中文"
        >
          中文
        </button>
        <button
          onclick={() => i18n.setLocale('en')}
          class="px-2 py-0.5 rounded text-xs font-mono transition cursor-pointer {i18n.locale === 'en' ? 'bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 shadow-2xs font-bold' : 'text-zinc-400 hover:text-zinc-600'}"
          title="English"
        >
          EN
        </button>
      </div>
    </div>

    <!-- Theme toggle -->
    <div class="flex items-center justify-between px-1">
      <span class="text-xs text-zinc-500 font-mono">{t('common.appearance')}</span>
      <div class="flex items-center gap-1 bg-zinc-200/60 dark:bg-zinc-900 p-0.5 rounded-md border border-zinc-200 dark:border-zinc-800">
        <button
          onclick={() => theme.setMode('light')}
          class="p-1.5 rounded text-xs transition cursor-pointer {theme.currentMode === 'light' ? 'bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 shadow-2xs' : 'text-zinc-400 hover:text-zinc-600'}"
          title="Light mode"
        >
          <Sun class="w-4 h-4" />
        </button>
        <button
          onclick={() => theme.setMode('system')}
          class="p-1.5 rounded text-xs transition cursor-pointer {theme.currentMode === 'system' ? 'bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 shadow-2xs' : 'text-zinc-400 hover:text-zinc-600'}"
          title="System sync"
        >
          <Laptop class="w-4 h-4" />
        </button>
        <button
          onclick={() => theme.setMode('dark')}
          class="p-1.5 rounded text-xs transition cursor-pointer {theme.currentMode === 'dark' ? 'bg-white dark:bg-zinc-800 text-zinc-900 dark:text-zinc-100 shadow-2xs' : 'text-zinc-400 hover:text-zinc-600'}"
          title="Dark mode"
        >
          <Moon class="w-4 h-4" />
        </button>
      </div>
    </div>
  </div>
</aside>
