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

/**
 * The console's navigation, in Material 3's visual language at desktop density.
 *
 * Colours and states are Material's navigation roles: a secondary-container pill marks the current
 * page with on-secondary-container content, other destinations use on-surface-variant, and hover
 * and press lay an 8% / 10% on-secondary-container state layer over them. Sizes are the console's
 * own: 48px rows and 20px icons instead of the touch rail's 56–64px items, so ten destinations fit
 * a laptop screen without the rail turning into a column of loose islands.
 *
 * Expanded (256px) every destination is a full-width pill; collapsed (80px) it is Material's 56×32
 * indicator with the label moved into a tooltip. Both states share 12px of inner padding and one
 * icon column 40px from the rail's edge, and every row keeps its height, so collapsing changes the
 * width and the labels — nothing moves up, down or sideways.
 *
 * The mobile drawer renders the expanded form; its toggle closes the drawer instead.
 */
let {
  onOpenCommand,
  onNavigate,
  collapsed = false,
  onToggle,
} = $props<{
  onOpenCommand: () => void;
  /** Called after a navigation so the mobile drawer can close itself. */
  onNavigate?: () => void;
  /** The 80px icon rail; labels move into tooltips. */
  collapsed?: boolean;
  /** The header's toggle: collapses or expands the rail, or closes the drawer. */
  onToggle: () => void;
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

const toggleLabel = $derived(
  collapsed ? t('shell.expand_nav') : t('shell.collapse_nav'),
);

function go(page: Page) {
  router.navigate(page);
  onNavigate?.();
}
</script>

<!-- 44px of top space centres the 48px header row 68px from the window top, the line the page
     title is centred on (see `App.svelte`), so the rail header and the page header read as one. -->
<div class="rail-nav flex h-full flex-col px-3 pt-11 pb-4" class:collapsed>
  <div class="flex h-12 shrink-0 items-center {collapsed ? 'justify-center' : 'gap-2 pl-[18px]'}">
    {#if !collapsed}
      <div class="flex min-w-0 flex-1 flex-col whitespace-nowrap">
        <span class="text-[22px] leading-7 font-semibold tracking-[-0.01em]">Kanon Console</span>
        <span class="flex items-center gap-1.5 text-[12px] leading-4 text-fg2">
          <i class="dot {nodeStore.error ? 'dot-bad' : nodeStore.health ? 'dot-ok' : 'dot-warn'}"></i>
          <span class="truncate">{statusText}</span>
        </span>
      </div>
    {/if}
    <!-- Google's icon button renders a native <button> in its shadow root, which provides the role
         and Enter/Space activation; Svelte cannot see into it, so its a11y lint misfires here. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-gb-icon-button
      class="shrink-0"
      color="standard"
      size="sm"
      type="button"
      aria-label={toggleLabel}
      aria-expanded={!collapsed}
      title={toggleLabel}
      onclick={onToggle}
    >
      {#if collapsed}
        <PanelLeftOpen size={20} strokeWidth={1.75} />
      {:else}
        <PanelLeftClose size={20} strokeWidth={1.75} />
      {/if}
    </md-gb-icon-button>
  </div>

  <button
    type="button"
    onclick={onOpenCommand}
    title={collapsed ? `${t('shell.search')} (Ctrl K)` : undefined}
    aria-label={collapsed ? t('shell.search') : undefined}
    class="state-layer mt-3 flex h-10 shrink-0 items-center rounded-full bg-sunk whitespace-nowrap text-fg2 {collapsed
      ? 'w-14 justify-center'
      : 'gap-3 pr-4 pl-[18px]'}"
  >
    <Search size={20} strokeWidth={1.75} class="shrink-0" />
    {#if !collapsed}
      <span class="text-[14px] leading-5">{t('shell.search')}</span>
      <kbd class="ml-auto font-sans text-[12px] text-fg3">Ctrl K</kbd>
    {/if}
  </button>

  <!-- The list scrolls inside its own 12px gutter so focus rings and pills are never clipped. -->
  <nav
    class="scroll-thin -mx-3 mt-5 flex min-h-0 flex-1 flex-col gap-2 overflow-x-hidden overflow-y-auto px-3"
    aria-label={t('shell.nav')}
  >
    {#each groups as group, index (index)}
      <div class="flex flex-col gap-0.5">
        {#each group as item (item.id)}
          {@const Icon = item.icon}
          {@const on = router.page === item.id}
          {@const label = t(`nav.${item.id}`)}
          {@const trouble = item.id === 'platforms' && platformTrouble}
          {#snippet icon()}
            <span class="relative flex shrink-0">
              <Icon size={20} strokeWidth={on ? 2 : 1.75} />
              <!-- Material's small badge: a 6px dot on the icon's top-end corner. -->
              {#if trouble}
                <i
                  class="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-warn"
                  title={t('shell.platform_trouble')}
                  aria-label={t('shell.platform_trouble')}
                ></i>
              {/if}
            </span>
          {/snippet}
          <a
            href={item.id === 'home' ? '#/' : `#/${item.id}`}
            aria-current={on ? 'page' : undefined}
            title={collapsed ? label : undefined}
            aria-label={collapsed ? label : undefined}
            onclick={(e) => {
              e.preventDefault();
              go(item.id);
            }}
            class="flex h-12 shrink-0 items-center rounded-full text-[14px] leading-5 font-medium whitespace-nowrap no-underline {collapsed
              ? 'w-14 justify-center'
              : `indicator state-layer gap-3 pr-4 pl-[18px] ${on ? 'bg-accent-tint' : ''}`} {on
              ? 'text-accent-fg'
              : 'text-fg2'}"
          >
            {#if collapsed}
              <span
                class="indicator state-layer flex h-8 w-14 items-center justify-center rounded-full {on
                  ? 'bg-accent-tint'
                  : ''}"
              >
                {@render icon()}
              </span>
            {:else}
              {@render icon()}
              <span>{label}</span>
            {/if}
          </a>
        {/each}
      </div>
    {/each}
  </nav>
</div>

<style>
  /* Material's state layers: on-secondary-container at 8% on hover and 10% when pressed or
     keyboard-focused, laid over the pill without replacing its colour. A collapsed row is the whole
     56×48 cell, so hovering anywhere on it lights its indicator. */
  .rail-nav :global(.state-layer) {
    background-image: linear-gradient(
      color-mix(in srgb, var(--k-accent-fg) var(--state, 0%), transparent),
      color-mix(in srgb, var(--k-accent-fg) var(--state, 0%), transparent)
    );
  }
  .rail-nav :global(.state-layer:hover),
  .rail-nav a:hover :global(.state-layer) {
    --state: 8%;
  }
  .rail-nav :global(.state-layer:active),
  .rail-nav a:active :global(.state-layer),
  .rail-nav :global(.state-layer:focus-visible),
  .rail-nav a:focus-visible :global(.state-layer) {
    --state: 10%;
  }

  /* Material's focus indicator: a 3px secondary outline 2px outside the shape it belongs to — the
     search field, the expanded pill, or the collapsed row's indicator rather than its whole cell. */
  .rail-nav a:focus-visible,
  .rail-nav button:focus-visible {
    outline: 3px solid var(--md-sys-color-secondary);
    outline-offset: 2px;
  }
  .rail-nav.collapsed a:focus-visible {
    outline: none;
  }
  .rail-nav.collapsed a:focus-visible :global(.indicator) {
    outline: 3px solid var(--md-sys-color-secondary);
    outline-offset: 2px;
  }
</style>
