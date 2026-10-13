<script lang="ts">
import {
  Activity,
  Boxes,
  Cpu,
  Drama,
  House,
  MessageCircle,
  MessagesSquare,
  PanelLeftClose,
  PanelLeftOpen,
  Plug,
  Puzzle,
  Search,
  Settings,
} from 'lucide-svelte';
import { formatDuration } from '../../format';
import { t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { type Page, router } from '../../stores/router.svelte';
import type { IconComponent } from '../../types';

let {
  onOpenCommand,
  onNavigate,
  collapsed = false,
  onToggleCollapsed,
} = $props<{
  onOpenCommand: () => void;
  /** Called after a navigation so the mobile drawer can close itself. */
  onNavigate?: () => void;
  /** Icon-only rail: labels move into tooltips and accessible names. */
  collapsed?: boolean;
  /** Shows the collapse toggle; the mobile drawer leaves it out because it closes instead. */
  onToggleCollapsed?: () => void;
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
    { id: 'models', icon: Cpu },
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

const dotClass = $derived(
  nodeStore.error ? 'dot-bad' : nodeStore.health ? 'dot-ok' : 'dot-warn',
);

// Rows share one geometry in both states: 48px tall, the icon 16px in from the row's left edge
// when labelled and centred in a 48px circle when collapsed, so icons barely move on toggle.
const rowClass = $derived(
  collapsed
    ? 'relative w-12 justify-center px-0'
    : 'gap-3 px-4',
);

function go(page: Page) {
  router.navigate(page);
  onNavigate?.();
}
</script>

<div class="flex h-full flex-col gap-5 px-3 pt-[26px] pb-5">
  <!-- The header keeps its height when collapsed, so the search and the navigation stay put. -->
  {#if collapsed}
    <div
      class="flex h-[55px] w-12 flex-col items-center gap-[3px]"
      title={`Kanon Console · ${statusText}`}
    >
      <span class="text-[26px] leading-8 font-bold" aria-hidden="true">K</span>
      <i class="dot {dotClass} mt-[6px]" role="img" aria-label={statusText}></i>
    </div>
  {:else}
    <div class="flex flex-col gap-[3px] px-4">
      <span class="text-[26px] leading-8 font-bold tracking-[-0.01em] whitespace-nowrap">Kanon Console</span>
      <span class="flex items-center gap-[7px] text-[13px] whitespace-nowrap text-fg2">
        <i class="dot {dotClass}"></i>
        <span class="truncate">{statusText}</span>
      </span>
    </div>
  {/if}

  <button
    type="button"
    onclick={onOpenCommand}
    title={collapsed ? `${t('shell.search')} (Ctrl K)` : undefined}
    aria-label={collapsed ? t('shell.search') : undefined}
    class="flex h-10 shrink-0 items-center rounded-full bg-sunk text-[14px] whitespace-nowrap text-fg2 hover:text-fg {collapsed
      ? 'w-12 justify-center'
      : 'gap-2.5 px-4'}"
  >
    <Search size={16} strokeWidth={2} class="shrink-0" />
    {#if !collapsed}
      <span>{t('shell.search')}</span>
      <kbd class="ml-auto font-sans text-[12px] text-fg3">Ctrl K</kbd>
    {/if}
  </button>

  <nav class="scroll-thin -mx-1 flex min-h-0 flex-1 flex-col gap-2 overflow-x-hidden overflow-y-auto px-1" aria-label={t('shell.nav')}>
    {#each groups as group, index (index)}
      <div class="flex flex-col gap-0.5">
        {#each group as item (item.id)}
          {@const Icon = item.icon}
          {@const on = router.page === item.id}
          {@const label = t(`nav.${item.id}`)}
          <a
            href={item.id === 'home' ? '#/' : `#/${item.id}`}
            aria-current={on ? 'page' : undefined}
            title={collapsed ? label : undefined}
            aria-label={collapsed ? label : undefined}
            onclick={(e) => {
              e.preventDefault();
              go(item.id);
            }}
            class="flex h-12 shrink-0 items-center rounded-full text-[14px] font-medium whitespace-nowrap no-underline transition-colors {rowClass} {on
              ? 'bg-accent-tint text-accent-fg'
              : 'text-fg2 hover:bg-fg/8 hover:text-fg'}"
          >
            <Icon size={18} strokeWidth={2} class="shrink-0" />
            {#if !collapsed}<span>{label}</span>{/if}
            {#if item.id === 'platforms' && platformTrouble}
              <i
                class="h-2 w-2 rounded-full bg-warn {collapsed ? 'absolute top-2.5 right-2.5' : 'ml-auto'}"
                title={t('shell.platform_trouble')}
                aria-label={t('shell.platform_trouble')}
              ></i>
            {/if}
          </a>
        {/each}
      </div>
    {/each}
  </nav>

  {#if onToggleCollapsed}
    {@const label = collapsed ? t('shell.expand_nav') : t('shell.collapse_nav')}
    <button
      type="button"
      onclick={onToggleCollapsed}
      aria-expanded={!collapsed}
      title={collapsed ? label : undefined}
      aria-label={collapsed ? label : undefined}
      class="flex h-12 shrink-0 items-center rounded-full text-[14px] font-medium whitespace-nowrap text-fg2 transition-colors hover:bg-fg/8 hover:text-fg {rowClass}"
    >
      {#if collapsed}
        <PanelLeftOpen size={18} strokeWidth={2} class="shrink-0" />
      {:else}
        <PanelLeftClose size={18} strokeWidth={2} class="shrink-0" />
        <span>{label}</span>
      {/if}
    </button>
  {/if}
</div>
