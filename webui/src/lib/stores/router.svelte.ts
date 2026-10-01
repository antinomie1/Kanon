/** Top-level pages of the console, in navigation order. */
export const PAGES = [
  'home',
  'chat',
  'instances',
  'sessions',
  'personas',
  'platforms',
  'models',
  'extensions',
  'activity',
  'settings',
] as const;
export type Page = (typeof PAGES)[number];

/**
 * Location of the console, kept in the URL hash (`#/instances/<id>`) so a page can be bookmarked,
 * reloaded and navigated with the browser's back button. The hash is the single source of truth:
 * `navigate` only writes it, and the `hashchange` listener updates the state.
 */
class Router {
  page = $state<Page>('home');
  /** Everything after the page segment, e.g. an instance id or a settings section. */
  param = $state<string | null>(null);

  constructor() {
    if (typeof window !== 'undefined') {
      this.read();
      window.addEventListener('hashchange', () => this.read());
    }
  }

  /** Goes to `page` (and optionally a sub-location such as an instance id). */
  navigate(page: Page, param?: string | null) {
    const hash = param
      ? `#/${page}/${encodeURIComponent(param)}`
      : page === 'home'
        ? '#/'
        : `#/${page}`;
    if (window.location.hash !== hash) {
      window.location.hash = hash;
    } else {
      this.read();
    }
  }

  /**
   * Changes only the sub-location without adding a history entry, for selections that are not
   * worth a "back" step (switching between instances in the list).
   */
  replaceParam(param: string | null) {
    const hash = param
      ? `#/${this.page}/${encodeURIComponent(param)}`
      : `#/${this.page}`;
    history.replaceState(null, '', hash);
    this.param = param;
  }

  private read() {
    const [rawPage, ...rest] = window.location.hash
      .replace(/^#\/?/, '')
      .split('/');
    const page = (PAGES as readonly string[]).includes(rawPage)
      ? (rawPage as Page)
      : 'home';
    this.page = page;
    const param = rest.join('/');
    this.param = param ? decodeURIComponent(param) : null;
  }
}

export const router = new Router();
