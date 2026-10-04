import en from "./locales/en";
import zh from "./locales/zh";

export type Locale = 'zh' | 'en';
export const dictionaries = { en, zh };


const LOCALE_KEY = 'kanon-locale';

/**
 * Saved language choice. Storage can be unavailable (private windows, blocked site data), and the
 * console must still render, so a failed read means "not chosen" and the browser language decides.
 */
function readLocale(): Locale | null {
  try {
    const saved = localStorage.getItem(LOCALE_KEY);
    return saved === 'en' || saved === 'zh' ? saved : null;
  } catch {
    return null;
  }
}

/** Remembers the language; if storage is blocked the choice still holds until the page reloads. */
function writeLocale(locale: Locale) {
  try {
    localStorage.setItem(LOCALE_KEY, locale);
  } catch {
    // Nothing to do: the current page already uses the chosen language.
  }
}

class I18nStore {
  locale = $state<Locale>('zh');

  constructor() {
    if (typeof window !== 'undefined') {
      const saved = readLocale();
      if (saved) {
        this.locale = saved;
      } else {
        const navLang = navigator.language.toLowerCase();
        this.locale = navLang.startsWith('zh') ? 'zh' : 'en';
      }
      document.documentElement.lang = this.locale === 'zh' ? 'zh-CN' : 'en';
    }
  }

  setLocale(l: Locale) {
    this.locale = l;
    if (typeof window !== 'undefined') {
      writeLocale(l);
      document.documentElement.lang = l === 'zh' ? 'zh-CN' : 'en';
    }
  }

  toggle() {
    this.setLocale(this.locale === 'zh' ? 'en' : 'zh');
  }

  t(key: string, params?: Record<string, string | number>): string {
    const dict = dictionaries[this.locale] || dictionaries.en;
    let text =
      (dict as Record<string, string>)[key] ??
      (dictionaries.en as Record<string, string>)[key] ??
      key;
    if (params) {
      for (const [k, v] of Object.entries(params)) {
        text = text.replace(new RegExp(`{${k}}`, 'g'), String(v));
      }
    }
    return text;
  }
}

export const i18n = new I18nStore();
export const t = (key: string, params?: Record<string, string | number>) =>
  i18n.t(key, params);
