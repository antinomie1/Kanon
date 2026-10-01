<script lang="ts">
import { ChevronDown } from 'lucide-svelte';
import type { Snippet } from 'svelte';
import type { HTMLSelectAttributes } from 'svelte/elements';

/**
 * Native `<select>` in the console's input style with its own chevron.
 *
 * The native element is kept on purpose: it brings keyboard handling, screen-reader support and
 * the platform picker on phones for free.
 */
let {
  value = $bindable(),
  children,
  class: extra = '',
  ...rest
}: HTMLSelectAttributes & { children: Snippet } = $props();
</script>

<div class="relative min-w-0 {extra}">
  <select
    bind:value
    {...rest}
    class="input appearance-none pr-10 truncate"
  >
    {@render children()}
  </select>
  <ChevronDown
    size={16}
    strokeWidth={2.2}
    class="pointer-events-none absolute right-3.5 top-1/2 -translate-y-1/2 text-fg3"
  />
</div>
