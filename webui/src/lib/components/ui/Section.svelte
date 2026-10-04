<script lang="ts">
import { ChevronDown } from 'lucide-svelte';
import type { Snippet } from 'svelte';

/**
 * A settings section: what it is and why on the left, the controls on the right.
 *
 * The container decides how stacked sections are separated: hairlines inside an editor card,
 * gaps in the settings page's grouped list. On narrow screens the explanation moves above the
 * controls.
 */
let {
  title,
  hint,
  children,
  aside,
  collapsible = false,
  summary,
}: {
  title: string;
  /** Optional disclosure; closed initially so long settings do not dominate an editor. */
  collapsible?: boolean;
  /** Current configuration shown while the disclosure is closed. */
  summary?: string;
  hint?: string;
  children: Snippet;
  /** Extra content under the hint, such as an "inherited from" note. */
  aside?: Snippet;
} = $props();
</script>

{#if collapsible}
  <details class="group/section py-4">
    <summary class="flex cursor-pointer list-none items-center gap-3 rounded-lg py-2 marker:content-none focus-visible:outline-2 focus-visible:outline-accent">
      <span class="text-[16px] font-semibold">{title}</span>
      {#if summary}<span class="min-w-0 flex-1 truncate text-right text-[13px] text-fg2">{summary}</span>{/if}
      <ChevronDown size={18} class="ml-auto shrink-0 transition-transform group-open/section:rotate-180" />
    </summary>
    <div class="grid gap-x-8 gap-y-3 pt-4 md:grid-cols-[220px_minmax(0,1fr)]">
      <div class="min-w-0">
        {#if hint}<p class="m-0 hint">{hint}</p>{/if}
        {#if aside}{@render aside()}{/if}
      </div>
      <div class="flex min-w-0 flex-col gap-3.5">{@render children()}</div>
    </div>
  </details>
{:else}
<section
  class="grid gap-x-8 gap-y-3 py-6 md:grid-cols-[220px_minmax(0,1fr)]"
>
  <div class="min-w-0">
    <h3 class="m-0 mb-1 text-[16px] font-semibold">{title}</h3>
    {#if hint}<p class="m-0 hint">{hint}</p>{/if}
    {#if aside}{@render aside()}{/if}
  </div>
  <div class="flex min-w-0 flex-col gap-3.5">
    {@render children()}
  </div>
</section>

{/if}
