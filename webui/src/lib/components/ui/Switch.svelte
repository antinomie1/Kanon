<script lang="ts">
import { Check } from 'lucide-svelte';

/**
 * On/off switch, drawn by Google's Material 3 Expressive switch (`md-gb-switch`).
 *
 * The wrapper exists because the element and the console speak slightly different languages:
 *
 *   - the element's state is `selected`, the console's is `checked`;
 *   - the element fills its `on-icon` slot with the check mark that marks the "on" state without
 *     relying on colour, and the console draws icons with Lucide rather than Material Symbols;
 *   - the element re-dispatches `change` with `bubbles: true` but *not* `composed: true`, so that
 *     event never leaves the shadow root and no listener on this element can see it. A `click` does
 *     cross the boundary, and the element's own handler has already flipped its state by the time
 *     the click bubbles up here, so `selected` is the post-click truth.
 *
 * Call sites keep the original props and never learn which implementation sits underneath.
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

/** `selected` is the element's own property, which the DOM type for an unknown tag does not carry. */
let element = $state<(HTMLElement & { selected?: boolean }) | null>(null);

/**
 * Reports a click to the console, then hands control back to it.
 *
 * The read happens one microtask later on purpose. Google's switch flips the inner button's
 * `aria-checked` synchronously but moves its own `selected` in an after-dispatch hook, which runs
 * only once the click has finished propagating — so a listener on this host still sees the old
 * value while the click is in flight. One microtask later the click has settled (still in the same
 * task, so before the next paint) and `selected` is the truth.
 *
 * The element keeps its own state, so a console that rejects the change — or one like the instance
 * list that waits for the server before moving `checked` — would otherwise leave the switch showing
 * a state nobody accepted. Re-asserting the prop keeps this a controlled input: a parent that
 * accepted the change has already flipped `checked`, making the write a no-op, while a parent that
 * did not gets the old value back.
 */
function report() {
  const el = element;
  if (!el) return;
  queueMicrotask(() => {
    const next = el.selected ?? !checked;
    if (next !== checked) onchange(next);
    if (el.selected !== checked) el.selected = checked;
  });
}

/**
 * The listener is attached here instead of as an `onclick` attribute on the element because this
 * host is not itself interactive: the switch button lives in its shadow root and handles the
 * keyboard there (the element delegates focus to it, so space and enter activate it and its click
 * bubbles up). Svelte's accessibility analyser cannot look into a shadow root, so it reads a
 * template click handler as a static element with a click and says so; attaching the same event
 * imperatively keeps the warning list honest instead of suppressing a finding we do not agree with.
 */
$effect(() => {
  const el = element;
  if (!el) return;
  el.addEventListener('click', report);
  return () => el.removeEventListener('click', report);
});
</script>

<md-gb-switch bind:this={element} selected={checked} {disabled} aria-label={label} title={label}>
  <!-- `md-icon` is Google's own icon wrapper: the switch sizes and centres the handle from
       `--md-icon-size`, and a Lucide component cannot carry the `slot` attribute itself. -->
  <span slot="on-icon" class="md-icon"><Check size={16} strokeWidth={2.6} /></span>
</md-gb-switch>
