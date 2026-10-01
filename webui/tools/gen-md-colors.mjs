/**
 * Generates `src/lib/theme/md-colors.css`: the `--md-sys-color-*` token set that Google's Material
 * 3 Expressive components (`@material/web/labs/gb`) read, built from the very seeds the console's
 * own `--k-*` schemes come from.
 *
 * Everything is read out of the app instead of being duplicated here:
 *   - the accent list and picker order come from `src/lib/stores/theme.svelte.ts`;
 *   - the seed colour of each accent comes from the swatches in
 *     `src/lib/components/settings/AppearanceSettings.svelte`;
 *   - the accent whose scheme is the attribute-less `:root` set is `ACCENTS[0]`;
 *   - the existing `--k-*` values in `src/app.css` are used to assert that the generated roles
 *     still agree with the hand-checked scheme tokens.
 *
 * Colours are generated with `@material/material-color-utilities` 0.4.0 using spec version 2025
 * (the Material 3 Expressive colour spec): `SchemeTonalSpot` for the chromatic accents and
 * `SchemeMonochrome` for the neutral one. Those two choices are exactly what `app.css` documents,
 * so the two files stay in sync.
 *
 * Run from `webui/`:  bun tools/gen-md-colors.mjs
 *
 * The output is unlayered on purpose: it must win over Google's own `@layer md.sys.color` defaults
 * no matter which order the two stylesheets are imported in.
 */
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname } from 'node:path';
import {
  argbFromHex,
  hexFromArgb,
  Hct,
  MaterialDynamicColors,
  SchemeMonochrome,
  SchemeTonalSpot,
} from '@material/material-color-utilities';

const THEME_TS = 'src/lib/stores/theme.svelte.ts';
const APPEARANCE = 'src/lib/components/settings/AppearanceSettings.svelte';
const APP_CSS = 'src/app.css';
const OUT = 'src/lib/theme/md-colors.css';

/** How a generated role maps onto the console's own token, used for the drift assertion. */
const AUDIT = {
  surface: 'page',
  surfaceContainer: 'card',
  surfaceContainerHighest: 'sunk',
  outlineVariant: 'line',
  outline: 'outline',
  onSurface: 'fg',
  onSurfaceVariant: 'fg2',
  primary: 'accent',
  onPrimary: 'on-accent',
  secondaryContainer: 'accent-tint',
  onSecondaryContainer: 'accent-fg',
  tertiaryContainer: 'tertiary-tint',
  onTertiaryContainer: 'tertiary-fg',
  error: 'danger',
  onError: 'on-danger',
  errorContainer: 'danger-tint',
  onErrorContainer: 'danger-fg',
  inverseSurface: 'bar',
  inverseOnSurface: 'on-bar',
  inversePrimary: 'bar-accent',
};

/**
 * Roles the console deliberately does *not* take from Material: `app.css` replaces the error
 * container and its text with its own calmer recipe so warnings stop shouting (see the comment on
 * the colour schemes there). Those two are copied over from `app.css`, which stays their source of
 * truth, instead of being generated.
 */
const FROM_APP_CSS = {
  errorContainer: '--k-danger-tint',
  onErrorContainer: '--k-danger-fg',
};

/** camelCase role name -> kebab-case CSS custom property suffix. */
const kebab = (name) => name.replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();

/** Reads the accent order and the accent that owns the attribute-less base token set. */
function readAccents() {
  const ts = readFileSync(THEME_TS, 'utf8');
  const list = ts.match(/export const ACCENTS = \[([^\]]+)\] as const;/);
  if (!list) throw new Error(`${THEME_TS}: ACCENTS not found`);
  const accents = [...list[1].matchAll(/'([a-z]+)'/g)].map((m) => m[1]);
  if (accents.length < 2) throw new Error(`unexpected ACCENTS: ${list[1]}`);
  return { accents, base: accents[0] };
}

/** Reads the picker swatches, which are the seeds every colour scheme is generated from. */
function readSeeds() {
  const svelte = readFileSync(APPEARANCE, 'utf8');
  const block = svelte.match(/const SWATCH: Record<Accent, string> = \{([\s\S]*?)\};/);
  if (!block) throw new Error(`${APPEARANCE}: SWATCH not found`);
  return Object.fromEntries(
    [...block[1].matchAll(/(\w+): '(#[0-9a-f]{6})'/gi)].map((m) => [m[1], m[2]]),
  );
}

/** Reads the committed `--k-*` values per (accent, mode) so the generated roles can be audited. */
function readAppSchemes(base) {
  const css = readFileSync(APP_CSS, 'utf8');
  const schemes = {};
  for (const m of css.matchAll(/^ {2}(:root[^{]*?) \{\n([\s\S]*?)^ {2}\}\n/gm)) {
    if (!m[2].includes('--k-page:')) continue;
    const accent = /^:root(\.dark)?$/.test(m[1]) ? base : m[1].match(/"(\w+)"/)[1];
    const mode = m[1].includes('.dark') ? 'dark' : 'light';
    schemes[`${accent}/${mode}`] = Object.fromEntries(
      [...m[2].matchAll(/^ {4}(--k-[\w-]+): (#[0-9a-f]{6});/gm)].map((d) => [d[1], d[2]]),
    );
  }
  return schemes;
}

/** Every colour role the library exposes, filtered to real role objects. */
function roles() {
  return Object.getOwnPropertyNames(MaterialDynamicColors)
    .filter((name) => typeof MaterialDynamicColors[name]?.getArgb === 'function')
    .sort();
}

const { accents, base } = readAccents();
const seeds = readSeeds();
const committed = readAppSchemes(base);
const ROLE_NAMES = roles();

/** Renders one scheme as CSS custom properties. */
function schemeBlock(selectors, tokens) {
  const lines = Object.entries(tokens).map(([name, value]) => `  --md-sys-color-${kebab(name)}: ${value};`);
  return `${selectors.map((s) => `${s}`).join(',\n')} {\n${lines.join('\n')}\n}`;
}

/**
 * Builds the scheme for one accent and mode. Graphite is the neutral accent, so it uses the
 * monochrome scheme; every other accent uses the tonal-spot scheme Material generates for a seed.
 * Roles in `FROM_APP_CSS` are taken from the committed console tokens instead.
 */
function schemeFor(accent, dark, tokens) {
  const hct = Hct.fromInt(argbFromHex(seeds[accent]));
  const scheme =
    accent === base
      ? new SchemeMonochrome(hct, dark, 0, '2025')
      : new SchemeTonalSpot(hct, dark, 0, '2025');
  const generated = Object.fromEntries(
    ROLE_NAMES.map((name) => [name, hexFromArgb(MaterialDynamicColors[name].getArgb(scheme)).toLowerCase()]),
  );
  for (const [role, token] of Object.entries(FROM_APP_CSS)) {
    const value = tokens?.[token];
    if (!value) throw new Error(`${accent}/${dark ? 'dark' : 'light'}: app.css has no ${token}`);
    generated[role] = value;
  }
  return generated;
}

// Audit: the roles the console already committed by hand must still match the generator output.
const drift = [];
for (const accent of accents) {
  for (const dark of [false, true]) {
    const mode = dark ? 'dark' : 'light';
    const tokens = committed[`${accent}/${mode}`];
    if (!tokens) {
      drift.push(`${accent}/${mode}: no committed scheme in app.css`);
      continue;
    }
    const generated = schemeFor(accent, dark, tokens);
    for (const [role, token] of Object.entries(AUDIT)) {
      const want = generated[role];
      const got = tokens[`--k-${token}`];
      if (want !== got) drift.push(`${accent}/${mode} ${token}: app.css ${got} != generated ${want}`);
    }
  }
}
if (drift.length) {
  throw new Error(
    `generated roles disagree with app.css (regenerate both together):\n  ${drift.join('\n  ')}`,
  );
}

// Emit: base accent as the attribute-less set, then the rest in picker order.
const blocks = [];
for (const accent of [base, ...accents.filter((a) => a !== base)]) {
  for (const dark of [false, true]) {
    const mode = dark ? 'dark' : 'light';
    const selector =
      accent === base
        ? dark
          ? ':root.dark'
          : ':root'
        : `:root${dark ? '.dark' : ''}[data-accent="${accent}"]`;
    blocks.push(schemeBlock([selector], schemeFor(accent, dark, committed[`${accent}/${mode}`])));
  }
}

const body = `/*!
 * Generated by tools/gen-md-colors.mjs — do not edit by hand.
 *
 * The --md-sys-color-* roles Google's Material 3 Expressive components read, generated from the
 * same accent seeds as the console's own --k-* schemes (see src/app.css). Regenerate with
 * \`bun tools/gen-md-colors.mjs\` after changing an accent seed in AppearanceSettings.svelte; the
 * script refuses to write when the output stops agreeing with the committed --k-* values.
 *
 * ${accents.length} accents x 2 modes, spec version 2025 of the Material colour spec.
 */

${blocks.join('\n\n')}
`;

mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, body);
console.log(
  `${OUT}: ${accents.length} accents x 2 modes x ${ROLE_NAMES.length} roles (base accent: ${base})`,
);
