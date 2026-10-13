<script lang="ts" generics="T extends string">
import type { IconComponent } from '../../types';

/**
 * A set of mutually exclusive choices shown side by side, drawn as Material 3 Expressive's
 * connected button group — the component that replaces the segmented button, which M3 Expressive
 * deprecates ("Use instead of segmented button, which is deprecated", Google's button group docs).
 *
 * Each choice is Google's own button (`md-gb-button`) as an outlined toggle, the colour Google's
 * connected-group example uses, so the state layers, ripple, focus ring and the spring on shape
 * changes all come from the element. The group adds what `md.comp.button-group.connected.*`
 * specifies (rules in `app.css`): 2px between buttons, fully round outer corners, 8px inner corners
 * that turn fully round on the selected button and tighten to 4px while one is pressed. Selection
 * shows through shape and colour; connected groups carry no check mark.
 *
 * Sizes are the group's: `md` is the small group (40px), `sm` the extra-small one (32px). Like the
 * segmented set it replaces, the group fills a block container with equal columns and hugs its
 * widest label inside a flex row, so call sites keep their layout.
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

/**
 * The element flips its own `selected` once a click has been dispatched, unless the click was
 * cancelled. Cancelling it leaves selection to `value` alone: re-clicking the current choice
 * cannot clear it, and a parent that declines a change keeps showing the choice it kept.
 */
function pick(event: MouseEvent, next: T) {
  event.preventDefault();
  if (next !== value) onchange(next);
}
</script>

<div role="group" aria-label={label} class="kanon-cg">
  {#each options as option (option.value)}
    {@const Icon = option.icon}
    <!-- Google's button renders a native <button> in its shadow root, which provides the role and
         Enter/Space activation; Svelte cannot see into it, so its a11y lint misfires here. -->
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <md-gb-button
      type="toggle"
      color="outlined"
      size={size === 'sm' ? 'xs' : 'sm'}
      selected={option.value === value}
      class:kanon-cg-on={option.value === value}
      {disabled}
      title={option.title}
      aria-label={option.label ? undefined : option.title}
      onclick={(event: MouseEvent) => pick(event, option.value)}
    >
      {#if Icon}<Icon size={size === 'sm' ? 15 : 16} strokeWidth={2} />{/if}
      {option.label ?? ''}
    </md-gb-button>
  {/each}
</div>
