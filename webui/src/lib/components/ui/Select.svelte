<script lang="ts">
import { ChevronDown } from 'lucide-svelte';
import type { Snippet } from 'svelte';
import type { HTMLSelectAttributes } from 'svelte/elements';

/**
 * Native `<select>` in the console's field style with its own chevron.
 *
 * This is a deliberate exception to "use Google's component": `md-filled-select` was probed and
 * rejected, not skipped for convenience. Two findings, both measured in the browser:
 *
 *   1. Its field geometry has no density token of its own — the 56px comes from the shared `field`
 *      component and only moves via `--md-filled-field-top-space` / `-bottom-space` (the
 *      `--md-filled-select-text-field-*-space` names do not exist, and the text field's own space
 *      tokens do nothing here). Workable, but it means theming through another component's surface.
 *   2. **The selected value never renders in the field.** Four strategies all left the field showing
 *      nothing but its label: assigning `value` after the options were registered; marking the
 *      option `selected` (which additionally made the select report `value=""`); doing both; and
 *      assigning `headline` as a property instead of an attribute. Until that is understood,
 *      migrating the nine call sites would ship selects that look empty.
 *
 * The native element also brings keyboard handling, screen-reader support and the platform picker on
 * phones for free, which is what a dense settings form actually wants. Revisit only with a working
 * example of a programmatically selected value rendering in the field.
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
    strokeWidth={2}
    class="pointer-events-none absolute right-3.5 top-1/2 -translate-y-1/2 text-fg3"
  />
</div>
