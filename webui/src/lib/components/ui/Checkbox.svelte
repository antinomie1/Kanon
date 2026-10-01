<script lang="ts">
/**
 * Checkbox, drawn by Google's Material 3 Expressive checkbox (`md-gb-checkbox`).
 *
 * The element is a form-associated wrapper around a real `<input type="checkbox">` in its shadow
 * root, so it brings the ripple, the focus ring and the 18px box the console already used. Two
 * things need bridging:
 *
 *   - The element reports changes with an `input` event that is composed, so it reaches this host
 *     (unlike the switch, whose `change` event never crosses the shadow boundary). Listening here
 *     and writing the value back keeps the `checked` prop the single source of truth.
 *   - The console wraps every checkbox in a `<label>` whose text acts as the label. A label's
 *     activation only forwards a click to the *labelled control* — this host element — and a click
 *     dispatched on a host never reaches the `<input>` inside its shadow root, so clicking the text
 *     would silently do nothing. Clicks whose `composedPath()[0]` is the host are exactly those
 *     forwarded ones, and they are re-sent to the inner input; a click that started on the box
 *     itself keeps its own path and is left alone, which is what stops the box from toggling twice.
 */
let {
  checked = $bindable(false),
  disabled = false,
  label,
  onchange,
}: {
  checked?: boolean;
  disabled?: boolean;
  /** Accessible name. The visible text is usually the surrounding `<label>`. */
  label?: string;
  onchange?: (next: boolean) => void;
} = $props();

let element = $state<(HTMLElement & { checked?: boolean }) | null>(null);

$effect(() => {
  const el = element;
  if (!el) return;

  const onInput = () => {
    const next = el.checked ?? false;
    if (next !== checked) {
      checked = next;
      onchange?.(next);
    }
  };
  const onClick = (event: MouseEvent) => {
    if (event.composedPath()[0] !== el) return;
    (el.shadowRoot?.querySelector('input') as HTMLInputElement | null)?.click();
  };

  el.addEventListener('input', onInput);
  el.addEventListener('click', onClick);
  return () => {
    el.removeEventListener('input', onInput);
    el.removeEventListener('click', onClick);
  };
});
</script>

<md-gb-checkbox bind:this={element} {checked} {disabled} aria-label={label}></md-gb-checkbox>
