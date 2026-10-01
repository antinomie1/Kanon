<script lang="ts" generics="T extends string">
import type { IconComponent } from '../../types';

/**
 * Segmented control, drawn by Google's outlined segmented button set
 * (`md-outlined-segmented-button-set`): a small set of mutually exclusive choices shown side by
 * side, so the current choice and the alternatives are visible without opening anything.
 *
 * The set is light-DOM: each option is one `<md-outlined-segmented-button>` child, which is why the
 * wrapper renders them itself instead of taking a snippet. Two details the console needs:
 *
 *   - size. Google's segments are 40px, which is the console's `md`; `sm` sets the set's own
 *     `--md-outlined-segmented-button-container-height` to 32px.
 *   - selection. The set owns the selected state (`setButtonSelected(index, selected)`) and reports
 *     it with a `segmented-button-set-selection` event carrying the index, so the wrapper maps
 *     `value` to an index and back. A labelled segment gets Google's check mark on selection; an
 *     icon-only segment keeps its icon instead, because a check would replace the only thing
 *     identifying it.
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

let set = $state<(HTMLElement & { updateComplete?: Promise<unknown> }) | null>(
  null,
);

/**
 * Pushes the console's value into the set, which owns the selection.
 *
 * The buttons are set directly rather than through `setButtonSelected`, because the set refuses to
 * select a *disabled* button — which would leave a disabled control showing nothing instead of the
 * value it stands for. Writing the buttons keeps `value` authoritative in every case; clicks still
 * go through the set, whose own handler updates the buttons and then reports the index.
 */
$effect(() => {
  const el = set;
  const index = options.findIndex((option) => option.value === value);
  if (!el || index < 0) return;
  void el.updateComplete?.then(() => {
    const buttons = [
      ...el.querySelectorAll('md-outlined-segmented-button'),
    ] as (HTMLElement & {
      selected?: boolean;
    })[];
    buttons.forEach((button, i) => {
      if (button.selected !== (i === index)) button.selected = i === index;
    });
  });
});

/**
 * The set reports selection through its own event rather than through the buttons, so the index it
 * carries is translated back into the option's value. Clicks that re-select the current option are
 * ignored so `onchange` only ever fires for a real change.
 */
function onSelection(event: Event) {
  const detail = (event as CustomEvent<{ index: number; selected: boolean }>)
    .detail;
  if (!detail?.selected) return;
  const next = options[detail.index]?.value;
  if (next === undefined || next === value) return;
  value = next;
  onchange(next);
}
</script>

<md-outlined-segmented-button-set
  bind:this={set}
  class={size === 'sm' ? 'kanon-seg-sm' : ''}
  aria-label={label}
  onsegmented-button-set-selection={onSelection}
>
  {#each options as option (option.value)}
    {@const Icon = option.icon}
    <md-outlined-segmented-button
      label={option.label ?? ''}
      title={option.title}
      {disabled}
      noCheckmark={!option.label}
    >
      {#if Icon}
        <span slot="icon"><Icon size={size === 'sm' ? 15 : 16} strokeWidth={2} /></span>
      {/if}
    </md-outlined-segmented-button>
  {/each}
</md-outlined-segmented-button-set>
