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
  class="relative inline-block w-10 h-6 rounded-full shrink-0 transition-colors disabled:opacity-50 {checked
    ? 'bg-accent'
    : 'bg-[var(--k-switch-off)]'}"
>
  <span
    class="absolute top-[3px] left-[3px] w-[18px] h-[18px] rounded-full bg-white shadow-[0_1px_2px_rgb(0_0_0/0.2)] transition-transform {checked
      ? 'translate-x-4'
      : ''}"
  ></span>
</button>
