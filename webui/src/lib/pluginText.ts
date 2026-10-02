import { i18n } from './stores/i18n.svelte';
import type { PluginMeta } from './types';

/**
 * Display texts a plugin translated for the console's language.
 *
 * Plugins ship `i18n/<locale>.json` with BCP 47 tags (`zh-CN`, `en`), while the console only knows
 * `zh` and `en`, so a tag matches when it is the console's language or a region of it. An exact
 * match wins; otherwise the first regional variant in tag order is used, so the choice is stable.
 */
export function pluginTexts(
  plugin: Pick<PluginMeta, 'i18n'>,
): Record<string, string> {
  const locales = plugin.i18n ?? {};
  const want = i18n.locale;
  const tags = Object.keys(locales).sort();
  const tag =
    tags.find((tag) => tag.toLowerCase() === want) ??
    tags.find((tag) => tag.toLowerCase().split(/[-_]/)[0] === want);
  return tag ? locales[tag] : {};
}

/** A plugin's translated text for `key`, or `fallback` (the manifest's own text). */
export function pluginText(
  plugin: Pick<PluginMeta, 'i18n'>,
  key: string,
  fallback: string,
): string {
  const text = pluginTexts(plugin)[key];
  return text?.trim() ? text : fallback;
}
