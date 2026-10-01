<script lang="ts">
import type { Snippet } from 'svelte';
import type { HTMLInputAttributes } from 'svelte/elements';

/**
 * Text field, drawn by Google's filled text field (`md-filled-text-field`).
 *
 * The element carries the states the console would otherwise hand-write: hover and focus handling,
 * the active indicator that thickens on focus, the caret colour, disabled and error styling, the
 * `label`/`supporting-text` slots and form association. Three things are the console's own:
 *
 *   - density. Material's field is 56px; the console sits between that and its old 40px, so the
 *     default here is 48px (12px of space above and below a 24px input line) and `small` is the
 *     32px filter field. Both go through the field's own `--md-filled-text-field-*-space` tokens.
 *   - monospace, for identifiers, tokens and shell commands;
 *   - `spellcheck`, which the element has no property for (see below).
 *
 * The value travels both ways through the element's `input` event rather than `bind:value`, which
 * Svelte only allows on native form elements — the caller's own `oninput` is still called.
 *
 * Fields that need to be *resizable* multi-line editors (a prompt, a JSON blob, the chat composer)
 * stay native `<textarea class="input">`: `md-filled-text-field type="textarea"` cannot be resized
 * by the user, which those editors depend on.
 */
interface Props extends Omit<HTMLInputAttributes, 'size'> {
  value?: string;
  /** Monospace text, for identifiers, tokens and shell commands. */
  mono?: boolean;
  /** The 32px filter field instead of the 48px default. */
  small?: boolean;
  /** A pill-shaped filter field (the console's search boxes) instead of the squared-off fill. */
  pill?: boolean;
  /** Icon shown inside the field before the text (the search glyph). */
  leading?: Snippet;
  /** Icon or button shown inside the field after the text (the secret show/hide toggle). */
  trailing?: Snippet;
  /** Extra classes land on the element, which is where layout utilities belong. */
  class?: string;
}

let {
  value = $bindable(''),
  mono = false,
  small = false,
  pill = false,
  leading,
  trailing,
  spellcheck,
  oninput,
  class: extra = '',
  ...rest
}: Props = $props();

let element = $state<
  (HTMLElement & { updateComplete?: Promise<unknown> }) | null
>(null);

/** Writes the element's value back to the caller, then hands the event on. */
function handleInput(event: Event) {
  value = (event.currentTarget as unknown as { value: string }).value;
  (oninput as ((event: Event) => void) | undefined)?.(event);
}

/**
 * Google's field has no `spellcheck` property, so the attribute would land on the host element
 * while the browser checks the `<input>` inside its shadow root. The console turns spellcheck off
 * for command and token fields, so it is pushed onto the inner input once the field has rendered —
 * the same kind of small, documented shim the switch wrapper needs for its change event.
 */
$effect(() => {
  const el = element;
  if (!el || spellcheck === undefined) return;
  const wanted = spellcheck === true || spellcheck === 'true';
  void el.updateComplete?.then(() => {
    const inner = el.shadowRoot?.querySelector('input, textarea') as
      | HTMLInputElement
      | HTMLTextAreaElement
      | null;
    if (inner) inner.spellcheck = wanted;
  });
});

const classes = $derived(
  [
    'kanon-field',
    small ? 'kanon-field-sm' : '',
    mono ? 'kanon-field-mono' : '',
    pill ? 'kanon-field-pill' : '',
    extra,
  ]
    .filter(Boolean)
    .join(' '),
);
</script>

<md-filled-text-field
  bind:this={element}
  class={classes}
  {value}
  oninput={handleInput}
  {...rest}
>
  {#if leading}<span slot="leading-icon">{@render leading()}</span>{/if}
  {#if trailing}<span slot="trailing-icon">{@render trailing()}</span>{/if}
</md-filled-text-field>
