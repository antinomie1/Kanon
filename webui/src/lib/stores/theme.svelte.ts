export type ThemeMode = 'light' | 'dark' | 'system';

/**
 * Accent colours offered in Settings → Appearance, in picker order; each maps to a `data-accent`
 * token set in `app.css`. The first entry is the default: it is the attribute-less `:root` set.
 */
export const ACCENTS = ['graphite', 'violet', 'blue', 'teal', 'rose'] as const;
export type Accent = (typeof ACCENTS)[number];

const MODE_KEY = 'kanon-theme';
const ACCENT_KEY = 'kanon-accent';

/**
 * Reads a per-browser preference. Storage can be unavailable (private windows, blocked site data),
 * and the UI must still render, so a failed read simply means "not set".
 */
function readPref(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

/** Persists a per-browser preference; failure only loses the preference, never the current view. */
function writePref(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Storage is blocked: the choice still applies to this page until reload.
  }
}

/** Light/dark mode and accent colour, both per-browser and applied to `<html>`. */
class ThemeStore {
  private mode = $state<ThemeMode>('system');
  private isDark = $state(false);
  /** Accent for `<html>`; starts at the default, which is the attribute-less base token set. */
  private accentValue = $state<Accent>('graphite');

  constructor() {
    if (typeof window !== 'undefined') {
      const saved = readPref(MODE_KEY);
      if (saved === 'light' || saved === 'dark' || saved === 'system') {
        this.mode = saved;
      }
      const accent = readPref(ACCENT_KEY);
      if (accent && (ACCENTS as readonly string[]).includes(accent)) {
        this.accentValue = accent as Accent;
      }
      this.updateClass();
      this.applyAccent();

      window
        .matchMedia('(prefers-color-scheme: dark)')
        .addEventListener('change', () => {
          if (this.mode === 'system') {
            this.updateClass();
          }
        });
    }
  }

  public get currentMode(): ThemeMode {
    return this.mode;
  }

  public get dark(): boolean {
    return this.isDark;
  }

  public get accent(): Accent {
    return this.accentValue;
  }

  public setMode(mode: ThemeMode) {
    this.mode = mode;
    writePref(MODE_KEY, mode);
    this.updateClass();
  }

  public setAccent(accent: Accent) {
    this.accentValue = accent;
    writePref(ACCENT_KEY, accent);
    this.applyAccent();
  }

  public toggle() {
    this.setMode(this.isDark ? 'light' : 'dark');
  }

  private updateClass() {
    const prefersDark = window.matchMedia(
      '(prefers-color-scheme: dark)',
    ).matches;
    this.isDark =
      this.mode === 'dark' || (this.mode === 'system' && prefersDark);
    document.documentElement.classList.toggle('dark', this.isDark);
  }

  private applyAccent() {
    // Graphite is the base token set (the default accent), so it needs no attribute.
    if (this.accentValue === 'graphite') {
      document.documentElement.removeAttribute('data-accent');
    } else {
      document.documentElement.setAttribute('data-accent', this.accentValue);
    }
  }
}

export const theme = new ThemeStore();
