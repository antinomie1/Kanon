<script lang="ts">
import {
  Activity,
  Boxes,
  BrainCircuit,
  Drama,
  House,
  MessageCircle,
  MessagesSquare,
  Monitor,
  Moon,
  Plug,
  Puzzle,
  Search,
  Settings,
  Sun,
} from 'lucide-svelte';
import { formatDuration } from '../../format';
import { i18n, type Locale, t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { type Page, router } from '../../stores/router.svelte';
import { type ThemeMode, theme } from '../../stores/theme.svelte';
import type { IconComponent } from '../../types';
import Seg from '../ui/Seg.svelte';

let { onOpenCommand, onNavigate } = $props<{
  onOpenCommand: () => void;
  /** Called after a navigation so the mobile drawer can close itself. */
  onNavigate?: () => void;
}>();

// Groups follow what a person does: look and try; decide who answers and how; connect things;
// check what happened and adjust the node.
const groups: { id: Page; icon: IconComponent }[][] = [
  [
    { id: 'home', icon: House },
    { id: 'chat', icon: MessageCircle },
  ],
  [
    { id: 'instances', icon: Boxes },
    { id: 'sessions', icon: MessagesSquare },
    { id: 'personas', icon: Drama },
  ],
  [
    { id: 'platforms', icon: Plug },
    { id: 'models', icon: BrainCircuit },
    { id: 'extensions', icon: Puzzle },
  ],
  [
    { id: 'activity', icon: Activity },
    { id: 'settings', icon: Settings },
  ],
];

const platformTrouble = $derived(instancesStore.adapterProblems.length > 0);

const statusText = $derived.by(() => {
  if (nodeStore.error) return t('shell.node_unreachable');
  if (!nodeStore.health) return t('shell.node_connecting');
  return t('shell.node_running', {
    time: formatDuration(nodeStore.health.uptime_seconds),
  });
});

function go(page: Page) {
  router.navigate(page);
  onNavigate?.();
}
</script>

<div class="flex h-full flex-col gap-5 px-3 pt-[26px] pb-5">
  <div class="flex flex-col gap-[3px] px-4">
    <span class="text-[26px] leading-8 font-bold tracking-[-0.01em]">Kanon Console</span>
    <span class="flex items-center gap-[7px] text-[13px] whitespace-nowrap text-fg2">
      <i
        class="dot {nodeStore.error ? 'dot-bad' : nodeStore.health ? 'dot-ok' : 'dot-warn'}"
      ></i>
      <span class="truncate">{statusText}</span>
    </span>
  </div>

  <button
    type="button"
    onclick={onOpenCommand}
    class="flex h-10 items-center gap-2.5 rounded-full bg-sunk px-4 text-[14px] whitespace-nowrap text-fg2 hover:text-fg"
  >
    <Search size={16} strokeWidth={2} class="shrink-0" />
    <span>{t('shell.search')}</span>
    <kbd class="ml-auto font-sans text-[12px] text-fg3">Ctrl K</kbd>
  </button>

  <nav class="scroll-thin -mx-1 flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto px-1" aria-label={t('shell.nav')}>
    {#each groups as group, index (index)}
      <div class="flex flex-col gap-0.5">
        {#each group as item (item.id)}
          {@const Icon = item.icon}
          {@const on = router.page === item.id}
          <a
            href={item.id === 'home' ? '#/' : `#/${item.id}`}
            aria-current={on ? 'page' : undefined}
            onclick={(e) => {
              e.preventDefault();
              go(item.id);
            }}
            class="flex h-12 items-center gap-3 rounded-full px-4 text-[14px] font-medium whitespace-nowrap no-underline transition-colors {on
              ? 'bg-accent-tint text-accent-fg'
              : 'text-fg2 hover:bg-fg/8 hover:text-fg'}"
          >
            <Icon size={18} strokeWidth={2} class="shrink-0" />
            <span>{t(`nav.${item.id}`)}</span>
            {#if item.id === 'platforms' && platformTrouble}
              <i
                class="ml-auto h-2 w-2 rounded-full bg-warn"
                title={t('shell.platform_trouble')}
                aria-label={t('shell.platform_trouble')}
              ></i>
            {/if}
          </a>
        {/each}
      </div>
    {/each}
  </nav>

  <div class="flex items-center justify-between gap-2">
    <Seg
      size="sm"
      label={t('common.language')}
      value={i18n.locale}
      onchange={(next: Locale) => i18n.setLocale(next)}
      options={[
        { value: 'zh', label: '中文' },
        { value: 'en', label: 'EN' },
      ]}
    />
    <Seg
      size="sm"
      label={t('settings.theme')}
      value={theme.currentMode}
      onchange={(next: ThemeMode) => theme.setMode(next)}
      options={[
        { value: 'system', icon: Monitor, title: t('settings.theme_system') },
        { value: 'light', icon: Sun, title: t('settings.theme_light') },
        { value: 'dark', icon: Moon, title: t('settings.theme_dark') },
      ]}
    />
  </div>
</div>
