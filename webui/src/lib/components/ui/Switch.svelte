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
 *   - the element moves `selected` late: only once the click has finished its whole trip through
 *     the page (Google's hook listens on `window`), and then it re-sends `change` from this host.
 *     That `change` is the first moment `selected` holds the new state, so it is what is listened
 *     to. A host `click` listener is too early: a real mouse click runs microtasks between
 *     listeners, so even a deferred read still sees the old state and the switch drifts one click
 *     out of step with the console.
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
 * Reports the element's `change` to the console, then hands control back to it.
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
  const next = el.selected ?? !checked;
  if (next !== checked) onchange(next);
  if (el.selected !== checked) el.selected = checked;
}
</script>

<md-gb-switch
  bind:this={element}
  selected={checked}
  {disabled}
  aria-label={label}
  title={label}
  onchange={report}
>
  <!-- `md-icon` is Google's own icon wrapper: the switch sizes and centres the handle from
       `--md-icon-size`, and a Lucide component cannot carry the `slot` attribute itself. -->
  <span slot="on-icon" class="md-icon"><Check size={16} strokeWidth={2.6} /></span>
</md-gb-switch>
