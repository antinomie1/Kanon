<script lang="ts" generics="T extends string">
import type { IconComponent } from '../../types';

/**
 * Segmented control: a small set of mutually exclusive choices shown side by side.
 *
 * Used instead of a `<select>` whenever every option fits on one line, so the current choice and
 * the alternatives are visible without opening anything.
 */
let {
  options,
  value,
  onchange,
  label,
  size = 'md',
  disabled = false,
}: {
  options: { value: T; label?: string; icon?: IconComponent; title?: string }[];
  value: T;
  onchange: (next: T) => void;
  /** Accessible name of the group. */
  label: string;
  size?: 'sm' | 'md';
  disabled?: boolean;
} = $props();
</script>

<div
  role="radiogroup"
  aria-label={label}
  class="inline-flex max-w-full gap-0.5 rounded-xl bg-sunk p-[3px] {disabled ? 'opacity-50' : ''}"
>
  {#each options as option (option.value)}
    {@const Icon = option.icon}
    {@const on = option.value === value}
    <button
      type="button"
      role="radio"
      aria-checked={on}
      aria-label={option.label ? undefined : (option.title ?? option.value)}
      title={option.title}
      {disabled}
      onclick={() => onchange(option.value)}
      class="inline-flex items-center justify-center gap-1.5 whitespace-nowrap font-bold transition-colors {size ===
      'sm'
        ? 'h-[26px] text-[12.5px] rounded-[9px]'
        : 'h-[34px] text-[14px] rounded-[10px]'} {option.label
        ? size === 'sm'
          ? 'px-2.5'
          : 'px-4'
        : size === 'sm'
          ? 'w-[28px]'
          : 'w-[36px]'} {on
        ? 'bg-[var(--k-seg-on)] text-fg shadow-[0_1px_2px_rgb(34_32_44/0.1)]'
        : 'text-fg2 hover:text-fg'}"
    >
      {#if Icon}<Icon size={size === 'sm' ? 14 : 15} strokeWidth={2.2} />{/if}
      {#if option.label}<span>{option.label}</span>{/if}
    </button>
  {/each}
</div>
