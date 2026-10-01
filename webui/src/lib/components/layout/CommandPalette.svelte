<script lang="ts">
import {
  Activity,
  Boxes,
  CornerDownLeft,
  Cpu,
  Drama,
  House,
  Languages,
  MessageCircle,
  MessagesSquare,
  Moon,
  Plug,
  Plus,
  Puzzle,
  RefreshCw,
  Search,
  Settings,
  Sun,
} from 'lucide-svelte';
import { faded, popped } from '../../motion';
import { i18n, t } from '../../stores/i18n.svelte';
import { instancesStore } from '../../stores/instances.svelte';
import { nodeStore } from '../../stores/node.svelte';
import { type Page, router } from '../../stores/router.svelte';
import { theme } from '../../stores/theme.svelte';
import type { IconComponent } from '../../types';

let { isOpen = $bindable(false) } = $props<{ isOpen: boolean }>();

interface Command {
  id: string;
  title: string;
  group: string;
  icon: IconComponent;
  action: () => void;
}

let query = $state('');
let active = $state(0);
let input = $state<HTMLInputElement>();

const pages: { id: Page; icon: IconComponent }[] = [
  { id: 'home', icon: House },
  { id: 'chat', icon: MessageCircle },
  { id: 'instances', icon: Boxes },
  { id: 'sessions', icon: MessagesSquare },
  { id: 'personas', icon: Drama },
  { id: 'platforms', icon: Plug },
  { id: 'models', icon: Cpu },
  { id: 'extensions', icon: Puzzle },
  { id: 'activity', icon: Activity },
  { id: 'settings', icon: Settings },
];

const commands = $derived<Command[]>([
  ...pages.map((page) => ({
    id: `page:${page.id}`,
    title: t(`nav.${page.id}`),
    group: t('palette.pages'),
    icon: page.icon,
    action: () => router.navigate(page.id),
  })),
  ...instancesStore.instances.map((instance) => ({
    id: `instance:${instance.id}`,
    title: instance.name,
    group: t('palette.instances'),
    icon: Boxes,
    action: () => router.navigate('instances', instance.id),
  })),
  {
    id: 'new-instance',
    title: t('instances.new'),
    group: t('palette.actions'),
    icon: Plus,
    action: () => router.navigate('instances', 'new'),
  },
  {
    id: 'refresh',
    title: t('palette.refresh'),
    group: t('palette.actions'),
    icon: RefreshCw,
    action: () => {
      void nodeStore.refresh();
      void instancesStore.load();
    },
  },
  {
    id: 'language',
    title: i18n.locale === 'zh' ? 'Switch to English' : '切换到中文',
    group: t('palette.actions'),
    icon: Languages,
    action: () => i18n.toggle(),
  },
  {
    id: 'theme',
    title: theme.dark ? t('palette.theme_light') : t('palette.theme_dark'),
    group: t('palette.actions'),
    icon: theme.dark ? Sun : Moon,
    action: () => theme.toggle(),
  },
]);

const filtered = $derived.by(() => {
  const q = query.trim().toLowerCase();
  if (!q) return commands;
  return commands.filter(
    (c) => c.title.toLowerCase().includes(q) || c.id.toLowerCase().includes(q),
  );
});

$effect(() => {
  void query;
  active = 0;
});

$effect(() => {
  if (isOpen) {
    query = '';
    input?.focus();
  }
});

function run(command: Command | undefined) {
  if (!command) return;
  isOpen = false;
  command.action();
}

function onkeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    isOpen = false;
  } else if (e.key === 'ArrowDown') {
    e.preventDefault();
    active = Math.min(active + 1, filtered.length - 1);
  } else if (e.key === 'ArrowUp') {
    e.preventDefault();
    active = Math.max(active - 1, 0);
  } else if (e.key === 'Enter') {
    e.preventDefault();
    run(filtered[active]);
  }
}
</script>

{#if isOpen}
  <div class="fixed inset-0 z-50 flex items-start justify-center px-4 pt-[12vh]">
    <button
      type="button"
      class="absolute inset-0 cursor-default bg-black/32"
      aria-label={t('common.close')}
      tabindex="-1"
      onclick={() => (isOpen = false)}
      in:faded
      out:faded={{ duration: 100 }}
    ></button>
    <div
      class="relative flex w-full max-w-lg flex-col overflow-hidden rounded-[28px] bg-card shadow-[var(--k-pop)]"
      role="dialog"
      aria-modal="true"
      aria-label={t('shell.search')}
      in:popped
      out:popped={{ duration: 120 }}
    >
      <div class="flex items-center gap-3 border-b border-line px-5">
        <Search size={18} strokeWidth={2} class="shrink-0 text-fg3" />
        <input
          bind:this={input}
          bind:value={query}
          {onkeydown}
          type="text"
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-list"
          aria-activedescendant={filtered[active] ? `palette-${active}` : undefined}
          placeholder={t('palette.placeholder')}
          class="h-14 w-full bg-transparent text-[15.5px] text-fg outline-none placeholder:text-fg3"
        />
        <kbd class="shrink-0 font-sans text-[12px] text-fg3">Esc</kbd>
      </div>
      <div id="palette-list" role="listbox" class="scroll-thin max-h-[52vh] overflow-y-auto p-2">
        {#if filtered.length === 0}
          <p class="m-0 px-3 py-8 text-center text-[14px] text-fg2">{t('palette.empty')}</p>
        {:else}
          {#each filtered as command, index (command.id)}
            {@const Icon = command.icon}
            {#if index === 0 || filtered[index - 1].group !== command.group}
              <div class="px-3 pt-2.5 pb-1 text-[12.5px] font-medium text-fg3">{command.group}</div>
            {/if}
            <button
              id="palette-{index}"
              type="button"
              role="option"
              aria-selected={index === active}
              onmousemove={() => (active = index)}
              onclick={() => run(command)}
              class="flex h-11 w-full items-center gap-3 rounded-full px-4 text-left text-[14.5px] font-medium {index ===
              active
                ? 'bg-accent-tint text-accent-fg'
                : 'text-fg'}"
            >
              <Icon size={17} strokeWidth={2} class="shrink-0 {index === active ? 'text-accent' : 'text-fg3'}" />
              <span class="min-w-0 flex-1 truncate">{command.title}</span>
              {#if index === active}
                <CornerDownLeft size={15} strokeWidth={2} class="shrink-0 text-accent" />
              {/if}
            </button>
          {/each}
        {/if}
      </div>
    </div>
  </div>
{/if}
