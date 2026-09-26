import { api, ApiError } from '../api/client';
import type {
  MilkyConfig,
  MilkyConfigView,
  MilkyStatus,
  MilkyTestReport,
  MilkyTransport,
} from '../types';

/**
 * Console state for the Milky platform adapter.
 *
 * The form is deliberately a plain local copy of the node's stored configuration rather than a
 * live binding: an operator may type a new endpoint or credential and test it *before* committing,
 * and a rejected save must leave the form exactly as it was typed so the error can be corrected
 * instead of re-entered.
 */
class MilkyStore {
  /** Stored configuration as reported by the node, `null` until loaded. */
  config = $state<MilkyConfig | null>(null);
  /** Live connection status and counters. */
  status = $state<MilkyStatus | null>(null);
  /** Whether a credential is stored on the node. */
  tokenConfigured = $state(false);

  // Editable form fields.
  formEnabled = $state(false);
  formBaseUrl = $state('');
  formTransport = $state<MilkyTransport>('sse');
  /** Credential typed into the form; empty means "keep the stored one". */
  formToken = $state('');
  /** Explicit removal of the stored credential. */
  formClearToken = $state(false);

  loading = $state(false);
  saving = $state(false);
  /** Whether an immediate enable/disable request is in flight. */
  applyingEnabled = $state(false);
  testing = $state(false);
  error = $state<string | null>(null);
  message = $state<string | null>(null);
  testResult = $state<MilkyTestReport | null>(null);
  /** Set when this node does not host the adapter at all. */
  unavailable = $state(false);

  /**
   * i18n key of the current connection state.
   *
   * Exposed so the adapter list and its configuration panel describe the same adapter with the very
   * same words instead of each keeping its own copy of the mapping.
   */
  get stateLabelKey(): string {
    switch (this.status?.state) {
      case 'connected':
        return 'adapters.milky_state_connected';
      case 'connecting':
        return 'adapters.milky_state_connecting';
      case 'error':
        return 'adapters.milky_state_error';
      default:
        return 'adapters.milky_state_disabled';
    }
  }

  /** Visual tone of the current connection state. */
  get stateTone(): 'ok' | 'warn' | 'bad' | 'idle' {
    switch (this.status?.state) {
      case 'connected':
        return 'ok';
      case 'connecting':
        return 'warn';
      case 'error':
        return 'bad';
      default:
        return 'idle';
    }
  }

  /**
   * Whether the drawer holds a change the node has not been given yet.
   *
   * The adapter list's switch always reflects the node, while the panel's switch edits a form, so
   * the two can legitimately differ for a moment; this is what lets the panel say so instead of
   * looking out of sync by accident.
   */
  get hasPendingToggle(): boolean {
    const stored = this.config;
    return stored !== null && this.formEnabled !== stored.enabled;
  }

  /**
   * Enables or disables the adapter on the node immediately.
   *
   * This backs the switch in the adapter list, which is an action rather than a form field: an
   * operator must be able to stop a misbehaving platform without opening the panel and saving. Only
   * the fields this action owns are patched back, so a half-edited form in the drawer survives.
   */
  async setEnabled(enabled: boolean) {
    if (this.applyingEnabled) return;
    if (!this.config) {
      await this.load();
    }
    if (!this.config) return;

    this.applyingEnabled = true;
    this.error = null;
    try {
      const view = await api.updateMilkyConfig({
        enabled,
        platform: this.config.platform,
        display_name: this.config.display_name,
        base_url: this.config.base_url,
        transport: this.config.transport,
        // No credential fields: omitting them keeps the stored token.
      });
      this.config = view.config;
      this.status = view.status;
      this.tokenConfigured = view.status.token_configured;
      // Keep the panel's switch in step with the node.
      this.formEnabled = view.config.enabled;
    } catch (err) {
      this.error = err instanceof Error ? err.message : String(err);
    } finally {
      this.applyingEnabled = false;
    }
  }

  /**
   * Platform identifier the Milky adapter owns.
   *
   * Read from the loaded state so a renamed platform still matches its row in the adapter catalog,
   * and falling back to the default before the first load so the configuration entry is offered
   * without waiting for a round trip.
   */
  get platformId(): string {
    return this.config?.platform ?? this.status?.platform ?? 'milky';
  }

  /** Loads the stored configuration and the live status. */
  async load() {
    this.loading = true;
    this.error = null;
    try {
      const view = await api.getMilkyConfig();
      this.applyView(view);
      this.unavailable = false;
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        // The node runs without the adapter; the panel explains that instead of showing a form
        // that could never be saved.
        this.unavailable = true;
        this.config = null;
        this.status = null;
      } else {
        this.error = err instanceof Error ? err.message : String(err);
      }
    } finally {
      this.loading = false;
    }
  }

  /**
   * Loads the configuration on first use and refreshes only the status afterwards.
   *
   * Keeping the form out of the refresh path matters: a page-wide refresh must never discard a
   * half-typed endpoint or credential.
   */
  async ensureLoaded() {
    if (!this.config && !this.unavailable) {
      await this.load();
      return;
    }
    await this.refreshStatus();
  }

  /** Refreshes status only, keeping whatever the operator has typed in the form. */
  async refreshStatus() {
    try {
      const view = await api.getMilkyConfig();
      this.status = view.status;
      this.tokenConfigured = view.status.token_configured;
      this.unavailable = false;
    } catch (err) {
      if (err instanceof ApiError && err.status === 404) {
        this.unavailable = true;
      } else {
        this.error = err instanceof Error ? err.message : String(err);
      }
    }
  }

  /** Saves the form, applying and persisting it on the node. */
  async save() {
    if (!this.config) return;
    this.saving = true;
    this.error = null;
    this.message = null;

    try {
      const view = await api.updateMilkyConfig({
        enabled: this.formEnabled,
        platform: this.config.platform,
        display_name: this.config.display_name,
        base_url: this.formBaseUrl,
        transport: this.formTransport,
        // An empty form field keeps the stored credential; removal is requested explicitly.
        access_token: this.formClearToken ? undefined : this.formToken || undefined,
        clear_access_token: this.formClearToken,
      });
      this.applyView(view);
      this.message = 'saved';
    } catch (err) {
      this.error = err instanceof Error ? err.message : String(err);
    } finally {
      this.saving = false;
    }
  }

  /** Probes the endpoint with the current form values, without saving anything. */
  async test() {
    this.testing = true;
    this.error = null;
    this.testResult = null;

    try {
      this.testResult = await api.testMilkyConfig({
        base_url: this.formBaseUrl,
        access_token: this.formToken || undefined,
      });
    } catch (err) {
      this.error = err instanceof Error ? err.message : String(err);
    } finally {
      this.testing = false;
    }
  }

  /** Adopts a server view into the form, discarding typed-but-unsaved values. */
  private applyView(view: MilkyConfigView) {
    this.config = view.config;
    this.status = view.status;
    this.tokenConfigured = view.status.token_configured;

    this.formEnabled = view.config.enabled;
    this.formBaseUrl = view.config.base_url;
    this.formTransport = view.config.transport;
    this.formToken = '';
    this.formClearToken = false;
  }
}

export const milkyStore = new MilkyStore();
