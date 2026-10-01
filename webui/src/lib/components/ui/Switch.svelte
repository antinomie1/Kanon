<script lang="ts">
import { Check } from 'lucide-svelte';

/**
 * On/off switch.
 *
 * Rendered in two places for the same node setting — the adapter list and its configuration
 * panel — so it exists once: two hand-rolled copies of the same control drift apart visually and
 * silently disagree about accessibility attributes.
 *
 * Drawn as the Material 3 switch: off is an outlined track with a small thumb in the outline
 * colour; on fills the track with the accent and grows the thumb, which then carries a check so
 * the state does not depend on colour alone. Pressing swells the thumb further. The thumb's size
 * and position ride the shared spring, so it overshoots slightly and settles.
 */
let {
  checked = false,
  disabled = false,
  label,
  onchange,
}: {
  checked?: boolean;
  disabled?: boolean;
  /** Accessible name, also used as the tooltip. */
  label: string;
  onchange: (next: boolean) => void;
} = $props();
</script>

<button
  type="button"
  role="switch"
  aria-checked={checked}
  aria-label={label}
  title={label}
  {disabled}
  onclick={() => onchange(!checked)}
  class="group relative inline-block h-8 w-[52px] shrink-0 rounded-full transition-[background-color,box-shadow] duration-150 ease-[var(--ease-effects)] disabled:opacity-50 {checked
    ? 'bg-accent'
    : 'bg-sunk shadow-[inset_0_0_0_2px_var(--k-outline)]'}"
>
  <!-- Off: 16px at 8px from the left; on: 24px flush with a 4px inset on the right; pressed: 28px
       around the same centre. -->
  <span
    class="absolute top-1/2 grid -translate-y-1/2 place-items-center rounded-full transition-[left,width,height,background-color] duration-[350ms] ease-spring {checked
      ? 'left-6 size-6 bg-on-accent text-accent group-active:left-[22px] group-active:size-7'
      : 'left-2 size-4 bg-outline group-active:left-0.5 group-active:size-7'}"
  >
    {#if checked}<Check size={16} strokeWidth={2.6} />{/if}
  </span>
</button>
