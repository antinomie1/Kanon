<script lang="ts">
import type { Snippet } from 'svelte';
import type { IconComponent } from '../../types';

/** An empty list or panel: what is missing, and the action that fills it. */
let {
  icon: Icon,
  title,
  text,
  action,
  compact = false,
}: {
  icon?: IconComponent;
  title: string;
  text?: string;
  action?: Snippet;
  compact?: boolean;
} = $props();
</script>

<div class="flex flex-col items-center text-center {compact ? 'gap-2 px-4 py-8' : 'gap-3 px-6 py-14'}">
  {#if Icon}
    <div class="grid h-12 w-12 place-items-center rounded-full bg-sunk text-fg3">
      <Icon size={22} strokeWidth={2} />
    </div>
  {/if}
  <div class="max-w-[46ch]">
    <p class="m-0 text-[15.5px] font-extrabold">{title}</p>
    {#if text}<p class="m-0 mt-1 hint">{text}</p>{/if}
  </div>
  {#if action}<div class="mt-1 flex flex-wrap justify-center gap-2.5">{@render action()}</div>{/if}
</div>
