<script lang="ts">
import type { Snippet } from 'svelte';
import type { HTMLButtonAttributes } from 'svelte/elements';

/**
 * Button, drawn by Google's Material 3 Expressive button (`md-gb-button`).
 *
 * Google's element is the one that animates: it is a pill at rest and its corners spring in to
 * `--md-sys-shape-corner-md` (12px) while pressed, with the ripple, focus ring and state layers
 * coming from the same package. The wrapper keeps the console's vocabulary — a variant and a size,
 * the way the old `.btn` classes read — and carries what the element has no variant for:
 *
 *   - `danger`, `danger-filled`, `warn` and the three inverse-surface flavours, which remap
 *     Google's own colour roles on the host (the `.kanon-*` rules in `app.css`) rather than reaching into
 *     the shadow root;
 *   - the console's geometry: circular icon buttons, the 28px `xs` size (Google's smallest is
 *     32px), the 46px composer button and flush padding, all set through the exposed `btn` part;
 *   - `href`, which Google's element turns into a link button (`<a part="btn">`) by itself.
 *
 * Everything else — `disabled`, `type`, `title`, `aria-*`, `data-*`, `onclick` — is passed straight
 * through to the element, so call sites look the same as they did with `<button>`.
 */
export type ButtonVariant =
  | 'outlined'
  | 'filled'
  | 'tonal'
  | 'elevated'
  | 'text'
  | 'danger'
  | 'danger-filled'
  | 'warn'
  | 'inverse'
  | 'inverse-filled'
  | 'inverse-plain';
export type ButtonSize = 'md' | 'sm' | 'xs';

/** Variants that recolour a text button, or a filled one for `danger-filled` and `inverse-filled`. */
const COLORED: Partial<
  Record<ButtonVariant, { klass: string; color: string }>
> = {
  danger: { klass: 'kanon-danger', color: 'text' },
  'danger-filled': { klass: 'kanon-danger', color: 'filled' },
  warn: { klass: 'kanon-warn', color: 'text' },
  inverse: { klass: 'kanon-inverse', color: 'text' },
  'inverse-filled': { klass: 'kanon-inverse', color: 'filled' },
  'inverse-plain': { klass: 'kanon-inverse-plain', color: 'text' },
};

interface Props extends HTMLButtonAttributes {
  variant?: ButtonVariant;
  /** `md` is 40px (Google's `sm`), `sm` is 32px (Google's `xs`), `xs` is the console's 28px. */
  size?: ButtonSize;
  /**
   * Icon-only button: a circle whose width equals its height. Google's own `square` property is
   * deliberately *not* used for this — that one rounds the corners to 12px, while M3's icon buttons
   * (and the console's) are circles.
   */
  square?: boolean;
  /** Renders a link button instead of a form button. */
  href?: string;
  target?: string;
  rel?: string;
  /** Extra classes land on the element, which is where layout utilities belong. */
  class?: string;
  children?: Snippet;
}

let {
  variant = 'outlined',
  size = 'md',
  square = false,
  href,
  class: extra = '',
  children,
  ...rest
}: Props = $props();

/** The element's sizes are one step above the console's; `xs` additionally gets the part rule. */
const elementSize = $derived(size === 'md' ? 'sm' : 'xs');
const classes = $derived(
  [
    COLORED[variant]?.klass ?? '',
    size === 'xs' ? 'kanon-btn-xs' : '',
    square ? 'kanon-icon' : '',
    extra,
  ]
    .filter(Boolean)
    .join(' '),
);
const elementColor = $derived(COLORED[variant]?.color ?? variant);
</script>

<md-gb-button
  class={classes}
  size={elementSize}
  color={elementColor}
  {href}
  {...rest}
>
  {@render children?.()}
</md-gb-button>
