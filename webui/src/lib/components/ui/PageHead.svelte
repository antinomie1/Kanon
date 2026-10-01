<script lang="ts">
import type { Snippet } from 'svelte';

/** Page title with an optional one-line summary under it and actions on the right. */
let {
  title,
  sub,
  actions,
}: {
  title: string;
  sub?: Snippet;
  actions?: Snippet;
} = $props();
</script>

<header class="flex flex-wrap items-center justify-between gap-x-5 gap-y-3 px-1 pt-2">
  <div class="min-w-0">
    <h1 class="m-0 text-[36px] leading-[44px] font-[560] tracking-[-0.01em]">{title}</h1>
    {#if sub}
      <div class="mt-1.5 flex flex-wrap gap-x-6 gap-y-1 text-[14.5px] text-fg2">
        {@render sub()}
      </div>
    {/if}
  </div>
  {#if actions}
    <div class="flex flex-wrap items-center gap-2.5">{@render actions()}</div>
  {/if}
</header>
