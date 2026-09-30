import { api } from '../api/client';
import type { AdapterItem, Capability } from '../types';

/**
 * The node's adapters and the capabilities they declare.
 *
 * Every setting that depends on a platform feature asks this store which adapters support it, so
 * the console never hard-codes a platform list: a new adapter that declares a capability shows up
 * next to the matching settings on its own.
 */
class CapabilityStore {
  adapters = $state<AdapterItem[]>([]);
  loaded = $state(false);
  error = $state<string | null>(null);
  private pending: Promise<void> | null = null;

  /** Loads the catalog once; concurrent callers share the request. */
  ensureLoaded(): Promise<void> {
    if (this.loaded) return Promise.resolve();
    this.pending ??= this.load().finally(() => {
      this.pending = null;
    });
    return this.pending;
  }

  async load() {
    try {
      this.adapters = (await api.getAdapters()).adapters;
      this.loaded = true;
      this.error = null;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    }
  }

  /** Display names of the adapters declaring any of `capabilities`. */
  supporters(capabilities: Capability[]): string[] {
    return this.adapters
      .filter((adapter) =>
        capabilities.some((capability) =>
          adapter.capabilities?.includes(capability),
        ),
      )
      .map((adapter) => adapter.display_name);
  }
}

export const capabilityStore = new CapabilityStore();
