<script lang="ts" generics="T extends string">
import type { IconComponent } from '../../types';

/**
 * Segmented control: a small set of mutually exclusive choices shown side by side.
 *
 * Used instead of a `<select>` whenever every option fits on one line, so the current choice and
 * the alternatives are visible without opening anything.
 *
 * Drawn as one pill-shaped track holding every option, with the chosen one filled in the accent as
 * a smaller pill inside it, so the group reads as one control rather than a row of separate
 * buttons. Labels never gain or lose an icon on selection, so choosing an option does not shift
 * the row.
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

<!-- The track is 40px (32px small) tall with a 4px (3px) inset, so the chosen pill sits 32px
     (26px) tall and keeps an even ring of track around it. -->
<div
  role="radiogroup"
  aria-label={label}
  class="inline-flex max-w-full rounded-full bg-sunk {size === 'sm' ? 'h-8 p-[3px]' : 'h-10 p-1'} {disabled
    ? 'opacity-50'
    : ''}"
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
      class="inline-flex h-full items-center justify-center gap-1.5 rounded-full font-medium whitespace-nowrap transition-[background-color,color] duration-150 ease-[var(--ease-effects)] {size ===
      'sm'
        ? 'text-[12.5px]'
        : 'text-[14px]'} {option.label
        ? size === 'sm'
          ? 'px-2.5'
          : 'px-3.5'
        : size === 'sm'
          ? 'w-[30px]'
          : 'w-10'} {on
        ? 'bg-accent text-on-accent'
        : 'text-fg2 hover:bg-fg/6 hover:text-fg disabled:hover:bg-transparent disabled:hover:text-fg2'}"
    >
      {#if Icon}<Icon size={size === 'sm' ? 15 : 16} strokeWidth={2} />{/if}
      {#if option.label}<span>{option.label}</span>{/if}
    </button>
  {/each}
</div>
