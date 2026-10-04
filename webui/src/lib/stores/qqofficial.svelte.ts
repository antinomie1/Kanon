import { ApiError, api } from '../api/client';
import type {
  QqOfficialConfig,
  QqOfficialConfigView,
  QqOfficialStatus,
} from '../types';

/**
 * Console state for the built-in QQ Official adapter.
 *
 * The form is a local copy of the node's stored configuration rather than a live binding, so a
 * rejected save leaves exactly what the operator typed on screen to be corrected. The AppSecret is
 * write-only: the node never returns it, and an empty form field keeps the stored one.
 */
class QqOfficialStore {
  /** Stored configuration as reported by the node, `null` until loaded. */
  config = $state<QqOfficialConfig | null>(null);
  /** Live gateway state. */
  status = $state<QqOfficialStatus | null>(null);

  // Editable form fields.
  formEnabled = $state(false);
  formAppId = $state('');
  /** Secret typed into the form; empty means "keep the stored one". */
  formSecret = $state('');
  formSandbox = $state(false);
  formMarkdown = $state(false);

  loading = $state(false);
  saving = $state(false);
  /** Whether an immediate enable/disable request is in flight. */
  applyingEnabled = $state(false);
  error = $state<string | null>(null);
  message = $state<string | null>(null);
  /** Set when this node does not host the adapter at all. */
  unavailable = $state(false);

  /** Platform identifier of the adapter's row in the adapter catalog. */
  readonly platformId = 'qqofficial';

  /** i18n key of the current connection state, shared by the adapter list and the panel. */
  get stateLabelKey(): string {
    return `adapters.qq_state_${this.status?.connection_state ?? 'disabled'}`;
  }

  /** Visual tone of the current gateway state. */
  get stateTone(): 'ok' | 'warn' | 'bad' | 'idle' {
    switch (this.status?.connection_state) {
      case 'connected':
        return 'ok';
      case 'connecting':
        return 'warn';
      case 'disconnected':
        return 'bad';
      default:
        return 'idle';
    }
  }

  /** Whether the panel's switch differs from what the node runs. */
  get hasPendingToggle(): boolean {
    return this.config !== null && this.formEnabled !== this.config.enabled;
  }

  /** Loads the stored configuration and the live status into the form. */
  async load() {
    this.loading = true;
    this.error = null;
    try {
      this.applyView(await api.getQqOfficialConfig());
      this.unavailable = false;
    } catch (err) {
      this.handleLoadError(err);
    } finally {
      this.loading = false;
    }
  }

  /** Loads on first use and refreshes only the status afterwards, keeping a half-typed form. */
  async ensureLoaded() {
    if (!this.config && !this.unavailable) {
      await this.load();
      return;
    }
    try {
      this.status = (await api.getQqOfficialConfig()).status;
      this.unavailable = false;
    } catch (err) {
      this.handleLoadError(err);
    }
  }

  /**
   * Enables or disables the adapter immediately, backing the switch in the adapter list.
   *
   * Only the enabled flag is sent with the stored values, so a half-edited form in the drawer
   * survives.
   */
  async setEnabled(enabled: boolean) {
    if (this.applyingEnabled) return;
    if (!this.config) await this.load();
    if (!this.config) return;

    this.applyingEnabled = true;
    this.error = null;
    try {
      const view = await api.updateQqOfficialConfig({ enabled });
      this.config = view.config;
      this.status = view.status;
      this.formEnabled = view.config.enabled;
    } catch (err) {
      this.error = err instanceof Error ? err.message : String(err);
    } finally {
      this.applyingEnabled = false;
    }
  }

  /** Saves the form, applying and persisting it on the node. */
  async save() {
    this.saving = true;
    this.error = null;
    this.message = null;
    try {
      const view = await api.updateQqOfficialConfig({
        enabled: this.formEnabled,
        app_id: this.formAppId,
        sandbox: this.formSandbox,
        markdown: this.formMarkdown,
        secret: this.formSecret || undefined,
      });
      this.applyView(view);
      this.message = 'saved';
    } catch (err) {
      this.error = err instanceof Error ? err.message : String(err);
    } finally {
      this.saving = false;
    }
  }

  private handleLoadError(err: unknown) {
    if (err instanceof ApiError && err.status === 404) {
      // The node runs without the adapter; the panel explains that instead of an unsavable form.
      this.unavailable = true;
      this.config = null;
      this.status = null;
    } else {
      this.error = err instanceof Error ? err.message : String(err);
    }
  }

  /** Adopts a server view into the form, discarding typed-but-unsaved values. */
  private applyView(view: QqOfficialConfigView) {
    this.config = view.config;
    this.status = view.status;
    this.formEnabled = view.config.enabled;
    this.formAppId = view.config.app_id;
    this.formSecret = '';
    this.formSandbox = view.config.sandbox;
    this.formMarkdown = view.config.markdown;
  }
}

export const qqofficialStore = new QqOfficialStore();
