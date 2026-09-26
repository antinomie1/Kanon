<script lang="ts">
/**
 * On/off switch.
 *
 * Rendered in two places for the same node setting — the adapter list and its configuration
 * panel — so it exists once: two hand-rolled copies of the same control drift apart visually and
 * silently disagree about accessibility attributes.
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
  class="relative w-11 h-6 rounded-full transition cursor-pointer shrink-0 disabled:opacity-50 disabled:cursor-not-allowed {checked
    ? 'bg-emerald-500'
    : 'bg-zinc-300 dark:bg-zinc-700'}"
>
  <span
    class="absolute top-0.5 left-0.5 w-5 h-5 rounded-full bg-white transition-transform {checked
      ? 'translate-x-5'
      : ''}"
  ></span>
</button>
