import { api } from '../api/client';
import type {
  DiscoverModelsResponse,
  ModelCapabilities,
  ModelSpec,
  ModelsResponse,
} from '../types';

/**
 * Capabilities assumed for a model the operator adds by hand.
 *
 * Text and tools default to enabled because that is what the node assumes for an unknown model, and
 * every other modality defaults to off: claiming vision an endpoint does not support surfaces as an
 * opaque upstream 400, while omitting a capability merely hides a feature that can be enabled.
 */
export const DEFAULT_CAPABILITIES: ModelCapabilities = {
  text: true,
  vision: false,
  audio: false,
  video: false,
  tool_calling: true,
  reasoning: false,
};

/** Capability flags in the order the console renders them. */
export const CAPABILITY_FLAGS: (keyof ModelCapabilities)[] = [
  'text',
  'vision',
  'audio',
  'video',
  'tool_calling',
  'reasoning',
];

/**
 * Console state for the per-model settings catalog.
 *
 * The catalog is keyed by the canonical `<provider>/<model>` reference rather than by the bare
 * model id, because the same weights reached through two endpoints can differ in context window
 * and modalities. Instances and the Playground pick their model from this single list.
 */
class ModelsStore {
  catalog = $state<ModelsResponse | null>(null);
  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  get models(): ModelSpec[] {
    return this.catalog?.models ?? [];
  }

  /** Provider names a reference may use, as reported by the node. */
  get providers(): string[] {
    return this.catalog?.providers ?? [];
  }

  /** Canonical reference the node answers with when an instance carries no override. */
  get defaultModel(): string | null {
    return this.catalog?.default_model ?? null;
  }

  /** Canonical `<provider>/<model-id>` reference of one entry. */
  referenceOf(spec: ModelSpec): string {
    return `${spec.provider}/${spec.model}`;
  }

  /** Canonical references served by one provider, for suggestion lists. */
  referencesFor(provider: string): string[] {
    return this.models
      .filter((spec) => spec.provider === provider)
      .map((spec) => this.referenceOf(spec));
  }

  /** A blank draft, used by the "add model" row and by inline editing. */
  emptySpec(provider?: string): ModelSpec {
    return {
      provider: provider ?? this.providers[0] ?? '',
      model: '',
      capabilities: { ...DEFAULT_CAPABILITIES },
      source: 'manual',
    };
  }

  async load() {
    this.loading = true;
    this.error = null;
    try {
      this.catalog = await api.getModels();
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  /**
   * Creates or replaces one catalog entry.
   *
   * The server answers with the whole refreshed catalog, so the list is replaced rather than
   * patched locally: a derived field the console guessed wrong would otherwise survive the save.
   */
  async upsert(spec: ModelSpec): Promise<boolean> {
    this.saving = true;
    this.error = null;
    try {
      this.catalog = await api.upsertModel(spec);
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }

  async remove(reference: string): Promise<boolean> {
    this.saving = true;
    this.error = null;
    try {
      this.catalog = await api.deleteModel({ reference });
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }

  /**
   * Asks the node to read a provider's model listing.
   *
   * `persist: true` stores the result in the catalog; entries the operator edited by hand are never
   * overwritten by that write, so a discovery refresh cannot silently undo a correction.
   */
  async discover(
    provider: string,
    persist: boolean,
  ): Promise<DiscoverModelsResponse | null> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      const res = await api.discoverModels({ provider, persist });
      if (persist) {
        // Refresh through `load` so the list reflects exactly what the node stored.
        await this.load();
      }
      return res;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return null;
    } finally {
      this.saving = false;
    }
  }
}

export const modelsStore = new ModelsStore();
