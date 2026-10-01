/**
 * Enter/exit motion for the components the console still owns.
 *
 * Google's web components animate themselves, but only with fixed duration/easing pairs (their
 * spring tokens are defined in the token set and consumed by no component — `grep -rl spring` over
 * the package finds none). Dialogs, toasts and the command palette are the parts we draw, so this
 * is where Material 3 Expressive's *spring* motion actually lands.
 *
 * The numbers are androidx's `ExpressiveMotionTokens` (the same source the audit verified against);
 * material-web's own token set currently disagrees (it lists 0.9/1400 for the fast spatial spring)
 * and consumes neither, so there is nothing to stay compatible with:
 *
 *   - fast spatial     damping 0.6, stiffness 800 — the visible settle, for small surfaces
 *   - default spatial  damping 0.8, stiffness 380 — larger surfaces, barely any overshoot
 *   - effects          damping 1.0, stiffness 1600 — colour and opacity, never bounces
 */

/** Damping ratio, stiffness and the time the spring needs to settle. */
export interface Spring {
  damping: number;
  stiffness: number;
  duration: number;
}

export const FAST_SPATIAL: Spring = {
  damping: 0.6,
  stiffness: 800,
  duration: 350,
};
export const DEFAULT_SPATIAL: Spring = {
  damping: 0.8,
  stiffness: 380,
  duration: 400,
};
export const EFFECTS: Spring = { damping: 1, stiffness: 1600, duration: 150 };

/**
 * Turns a spring into an easing function.
 *
 * This is the analytic solution of a damped harmonic oscillator driven to 1 (mass 1, so
 * `omega0 = sqrt(stiffness)`), normalised so that `ease(1)` is exactly 1. Underdamped springs
 * overshoot, so the eased value passes 1 — which is the whole point: a dialog that scales from 0.9
 * up to 1 with a small overshoot is what makes the motion read as Material 3 Expressive rather than
 * as a fixed curve.
 */
export function springEase({
  damping,
  stiffness,
  duration,
}: Spring): (t: number) => number {
  const w0 = Math.sqrt(stiffness);
  const z = damping;
  const at = (seconds: number): number => {
    if (z < 1) {
      const wd = w0 * Math.sqrt(1 - z * z);
      return (
        1 -
        Math.exp(-z * w0 * seconds) *
          (Math.cos(wd * seconds) + ((z * w0) / wd) * Math.sin(wd * seconds))
      );
    }
    // Critically damped: no oscillation, which is what the colour/opacity springs want.
    return 1 - Math.exp(-w0 * seconds) * (1 + w0 * seconds);
  };
  const settled = at(duration / 1000);
  return (t: number) =>
    t <= 0 ? 0 : t >= 1 ? 1 : at((t * duration) / 1000) / settled;
}

const fastEase = springEase(FAST_SPATIAL);
const defaultEase = springEase(DEFAULT_SPATIAL);
const effectsEase = springEase(EFFECTS);

/** Svelte transition options; `css` is driven by the spring above. */
interface Options {
  duration?: number;
  /** Scale the surface starts from, e.g. 0.9 for a dialog. */
  from?: number;
  /** Pixels to travel, for slides. */
  distance?: number;
  /** `scale` for a centred dialog, `slide` for a drawer coming off the edge. */
  mode?: 'scale' | 'slide';
}

/**
 * A dialog-sized surface appearing or leaving. Centred dialogs fade while scaling up from `from`;
 * a drawer slides in from the edge it is attached to. Either way the slower spatial spring drives
 * it, so a large panel settles without a visible bounce at the end.
 */
export function surfaced(
  node: Element,
  {
    duration = DEFAULT_SPATIAL.duration,
    from = 0.94,
    mode = 'scale',
  }: Options = {},
) {
  const distance = mode === 'slide' ? node.getBoundingClientRect().width : 0;
  return {
    duration,
    easing: defaultEase,
    css: (t: number) =>
      mode === 'slide'
        ? `transform: translateX(${distance * (1 - t)}px); opacity: ${t}`
        : `transform: scale(${from + (1 - from) * t}); opacity: ${t}`,
  };
}

/** The same for something small and nearer the eye (a palette, a menu), which may overshoot. */
export function popped(
  _node: Element,
  { duration = FAST_SPATIAL.duration, from = 0.92 }: Options = {},
) {
  return {
    duration,
    easing: fastEase,
    css: (t: number) =>
      `transform: scale(${from + (1 - from) * t}); opacity: ${t}`,
  };
}

/** Fades only, on the non-oscillating effects spring: scrims, tooltips and other flat surfaces. */
export function faded(
  _node: Element,
  { duration = EFFECTS.duration }: Options = {},
) {
  return { duration, easing: effectsEase, css: (t: number) => `opacity: ${t}` };
}

/** Slides in from below (or above, with a negative distance) while fading. */
export function risen(
  _node: Element,
  { duration = FAST_SPATIAL.duration, distance = 12 }: Options = {},
) {
  return {
    duration,
    easing: fastEase,
    css: (t: number) =>
      `transform: translateY(${distance * (1 - t)}px); opacity: ${t}`,
  };
}
