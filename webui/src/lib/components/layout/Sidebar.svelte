<script lang="ts">
import {
  Activity,
  Boxes,
  Cpu,
  Drama,
  House,
  ListIndentDecrease,
  Menu,
  MessageCircle,
  MessagesSquare,
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
 * The console's navigation, drawn as Material 3 Expressive's navigation rail.
 *
 * Collapsed it is the 96px rail: a menu button on top and each destination as a 56×32 active
 * indicator with a label-medium caption under the icon. Expanded it is the wide rail (256px, inside
 * the spec's 220–360px): the same menu button, then destinations as 56px label-large pills that hug
 * their content. One icon column is centred 48px from the rail's edge in both states — menu button,
 * search and every destination icon — so the icons never move sideways when the rail toggles, and
 * expanded labels share one 68px start line with the product name.
 *
 * The mobile drawer renders the expanded form; its menu button closes the drawer instead.
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
  /** The 96px icon rail; labels move under the icons. */
  collapsed?: boolean;
  /** The menu button: collapses or expands the rail, or closes the drawer. */
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

<!-- 38px of top space centres the 56px header row on the page title's 44px line (the sheet is
     inset 12px and its content starts 32px lower), so rail and page read as one header band. -->
<div class="rail-nav flex h-full flex-col pt-[38px] pb-4">
  <div class="flex h-14 shrink-0 items-center pl-7">
    <!-- Google's icon button renders a native <button> in its shadow root, which provides the role
         and Enter/Space activation; Svelte cannot see into it, so its a11y lint misfires here. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-gb-icon-button
      color="standard"
      size="sm"
      type="button"
      aria-label={toggleLabel}
      aria-expanded={!collapsed}
      title={toggleLabel}
      onclick={onToggle}
    >
      {#if collapsed}
        <Menu size={24} strokeWidth={1.75} />
      {:else}
        <ListIndentDecrease size={24} strokeWidth={1.75} />
      {/if}
    </md-gb-icon-button>
    {#if !collapsed}
      <div class="flex min-w-0 flex-col whitespace-nowrap">
        <span class="text-[22px] leading-7 font-semibold tracking-[-0.01em]">Kanon Console</span>
        <span class="flex items-center gap-1.5 text-[12px] leading-4 text-fg2">
          <i class="dot {nodeStore.error ? 'dot-bad' : nodeStore.health ? 'dot-ok' : 'dot-warn'}"></i>
          <span class="truncate">{statusText}</span>
        </span>
      </div>
    {/if}
  </div>

  <!-- Material's search bar: a full-round 56px surface-container-highest field. Its icon sits in the
       rail's icon column and its hint on the label line. -->
  <button
    type="button"
    onclick={onOpenCommand}
    title={collapsed ? `${t('shell.search')} (Ctrl K)` : undefined}
    aria-label={collapsed ? t('shell.search') : undefined}
    class="state-layer mt-3 ml-5 flex h-14 shrink-0 items-center rounded-full bg-sunk whitespace-nowrap text-fg2 {collapsed
      ? 'w-14 justify-center'
      : 'mr-5 gap-2 pr-5 pl-4'}"
  >
    <Search size={24} strokeWidth={1.75} class="shrink-0" />
    {#if !collapsed}
      <span class="text-[16px] leading-6">{t('shell.search')}</span>
      <kbd class="ml-auto font-sans text-[12px] text-fg3">Ctrl K</kbd>
    {/if}
  </button>

  <nav
    class="scroll-thin mt-3 flex min-h-0 flex-1 flex-col overflow-x-hidden overflow-y-auto {collapsed
      ? 'gap-3'
      : 'gap-2'}"
    aria-label={t('shell.nav')}
  >
    {#each groups as group, index (index)}
      <div class="flex flex-col {collapsed ? 'gap-1' : ''}">
        {#each group as item (item.id)}
          {@const Icon = item.icon}
          {@const on = router.page === item.id}
          {@const label = t(`nav.${item.id}`)}
          {@const trouble = item.id === 'platforms' && platformTrouble}
          {#snippet icon()}
            <span class="relative flex shrink-0">
              <Icon size={24} strokeWidth={on ? 2 : 1.75} />
              <!-- Material's small badge: a 6px dot on the icon's top-end corner. -->
              {#if trouble}
                <i
                  class="absolute -top-px -right-px size-1.5 rounded-full bg-warn"
                  title={t('shell.platform_trouble')}
                  aria-label={t('shell.platform_trouble')}
                ></i>
              {/if}
            </span>
          {/snippet}
          <a
            href={item.id === 'home' ? '#/' : `#/${item.id}`}
            aria-current={on ? 'page' : undefined}
            onclick={(e) => {
              e.preventDefault();
              go(item.id);
            }}
            class="group no-underline whitespace-nowrap {collapsed
              ? 'flex h-14 w-24 shrink-0 flex-col items-center gap-1 rounded-2xl pt-1 text-[12px] leading-4 font-medium tracking-[0.03125rem]'
              : 'state-layer ml-5 flex h-14 w-fit shrink-0 items-center gap-2 rounded-full pr-4 pl-4 text-[14px] leading-5 font-medium'} {on
              ? collapsed
                ? 'text-[var(--md-sys-color-secondary)]'
                : 'bg-accent-tint text-accent-fg'
              : 'text-fg2'}"
          >
            {#if collapsed}
              <span
                class="state-layer flex h-8 w-14 items-center justify-center rounded-full {on
                  ? 'bg-accent-tint text-accent-fg'
                  : ''}"
              >
                {@render icon()}
              </span>
              <span>{label}</span>
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
  /* Material's state layers: 8% of the content colour on hover, 10% when pressed, drawn over the
     indicator (or nothing) without replacing its colour. In the collapsed rail the whole 96px item
     is the target, so hovering anywhere on it lights its indicator. */
  .rail-nav :global(.state-layer) {
    background-image: linear-gradient(
      color-mix(in oklab, currentColor var(--state, 0%), transparent),
      color-mix(in oklab, currentColor var(--state, 0%), transparent)
    );
  }
  .rail-nav :global(.state-layer:hover),
  .rail-nav :global(a:hover .state-layer) {
    --state: 8%;
  }
  .rail-nav :global(.state-layer:active),
  .rail-nav :global(a:active .state-layer) {
    --state: 10%;
  }
  /* Material's focus indicator: a 3px secondary outline, 2px outside the shape. */
  .rail-nav a:focus-visible,
  .rail-nav button:focus-visible {
    outline: 3px solid var(--md-sys-color-secondary);
    outline-offset: 2px;
  }
</style>
