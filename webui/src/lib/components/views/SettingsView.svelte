<script lang="ts">
import {
  Bell,
  Bot,
  MessageCircleReply,
  Palette,
  Server,
  ShieldCheck,
  SquareTerminal,
  TextQuote,
} from 'lucide-svelte';
import { t } from '../../stores/i18n.svelte';
import { router } from '../../stores/router.svelte';
import type { IconComponent } from '../../types';
import AgentSettings from '../settings/AgentSettings.svelte';
import AppearanceSettings from '../settings/AppearanceSettings.svelte';
import BashSettings from '../settings/BashSettings.svelte';
import CommandSettings from '../settings/CommandSettings.svelte';
import ContextSettings from '../settings/ContextSettings.svelte';
import EventSettings from '../settings/EventSettings.svelte';
import NodeSettings from '../settings/NodeSettings.svelte';
import ReplySettings from '../settings/ReplySettings.svelte';
import PageHead from '../ui/PageHead.svelte';

const sections: { id: string; icon: IconComponent }[] = [
  { id: 'appearance', icon: Palette },
  { id: 'agent', icon: Bot },
  { id: 'replies', icon: MessageCircleReply },
  { id: 'context', icon: TextQuote },
  { id: 'events', icon: Bell },
  { id: 'commands', icon: ShieldCheck },
  { id: 'bash', icon: SquareTerminal },
  { id: 'node', icon: Server },
];

const current = $derived(
  sections.some((s) => s.id === router.param)
    ? (router.param as string)
    : 'appearance',
);
</script>

<PageHead title={t('nav.settings')} />

<div class="grid items-start gap-4 lg:grid-cols-[220px_minmax(0,1fr)]">
  <nav
    class="card scroll-thin flex gap-0.5 overflow-x-auto p-2 lg:flex-col"
    aria-label={t('nav.settings')}
  >
    {#each sections as section (section.id)}
      {@const Icon = section.icon}
      {@const on = current === section.id}
      <a
        href="#/settings/{section.id}"
        aria-current={on ? 'page' : undefined}
        class="flex h-10 shrink-0 items-center gap-2.5 rounded-full px-4 text-[14px] font-medium whitespace-nowrap no-underline transition-colors {on
          ? 'bg-accent-tint text-accent-fg'
          : 'text-fg2 hover:bg-fg/8 hover:text-fg'}"
      >
        <Icon size={17} strokeWidth={2} class="shrink-0" />
        {t(`settings.section_${section.id}`)}
      </a>
    {/each}
  </nav>

  <section class="group-list">
    {#if current === 'appearance'}
      <AppearanceSettings />
    {:else if current === 'agent'}
      <AgentSettings />
    {:else if current === 'replies'}
      <ReplySettings />
    {:else if current === 'context'}
      <ContextSettings />
    {:else if current === 'events'}
      <EventSettings />
    {:else if current === 'commands'}
      <CommandSettings />
    {:else if current === 'bash'}
      <BashSettings />
    {:else if current === 'node'}
      <NodeSettings />
    {/if}
  </section>
</div>
