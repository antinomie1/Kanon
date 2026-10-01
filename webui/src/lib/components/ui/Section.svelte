<script lang="ts">
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
}: {
  title: string;
  hint?: string;
  children: Snippet;
  /** Extra content under the hint, such as an "inherited from" note. */
  aside?: Snippet;
} = $props();
</script>

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
