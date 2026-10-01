import type { ModelSpec } from '../../types';

/** Name a person reads for a catalog model: its display name, else the id the endpoint uses. */
export function modelName(spec: ModelSpec): string {
  return spec.display_name?.trim() || spec.model;
}

/**
 * Token counts as people say them: `128000` → `128K`, `1048576` → `1M`.
 *
 * Rounded to one decimal at most, since the exact figure is in the editor for whoever needs it.
 */
export function compactTokens(n: number): string {
  if (n >= 1_000_000) return `${trim(n / 1_000_000)}M`;
  if (n >= 1_000) return `${trim(n / 1_000)}K`;
  return String(n);
}

function trim(value: number): string {
  return value.toFixed(1).replace(/\.0$/, '');
}

/**
 * Optional numeric field converted to the wire form.
 *
 * Blank means "not set" (`undefined`). Text that is not a number gives `null`, so the form can
 * say so instead of quietly saving the field as unset.
 */
export function optionalNumber(raw: string): number | undefined | null {
  const text = raw.trim();
  if (text === '') return undefined;
  const value = Number(text);
  return Number.isFinite(value) ? value : null;
}
