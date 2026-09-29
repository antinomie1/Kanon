import { api } from '../api/client';
import type {
  DiscoverModelsResponse,
  ProviderInfo,
  ProvidersCatalog,
  SystemConfig,
  TestProviderRequest,
  TestProviderResponse,
  UpsertProviderRequest,
} from '../types';
import { modelsStore } from './models.svelte';

/**
 * Console state for the node's provider endpoints.
 *
 * # Providers are endpoints, not defaults
 * A provider is only an endpoint (protocol, base URL, credential). Which model answers is a single
 * node-wide decision owned by {@link modelsStore} (`defaultModel`), so there is no "active" or
 * "default" provider here to disagree with it.
 *
 * # One source of truth
 * The endpoints live in the node's `data/system.json`; the browser keeps no copy. Every mutation
 * posts to the gateway and replaces the whole catalog with the response, so the console can never
 * describe a directory the running node does not have. The stored credential is never returned —
 * only whether one exists — which is why editing a provider leaves the key field blank and
 * omitting it keeps whatever the node already holds.
 */
class ProvidersStore {
  catalog = $state<ProvidersCatalog | null>(null);
  systemConfig = $state<SystemConfig | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);

  /** True while a provider mutation is in flight. */
  pending = $state(false);
  /** Failure of the last provider mutation, shown next to the form that caused it. */
  actionError = $state<string | null>(null);

  /** Name of the endpoint currently open in the editor. */
  selectedProviderName = $state<string>('');

  /** Connectivity test result per provider name. */
  providerTestResults = $state<Record<string, TestProviderResponse>>({});
  testingProvider = $state<string | null>(null);

  /** Sandbox model choice; empty means "let the node use its default model". */
  activeModel = $state<string>('');

  constructor() {
    this.load();
  }

  get providers(): ProviderInfo[] {
    return this.catalog?.providers ?? [];
  }

  get selectedProvider(): ProviderInfo | undefined {
    return (
      this.providers.find((p) => p.name === this.selectedProviderName) ??
      this.providers[0]
    );
  }

  /** Catalog model references served by one endpoint, used as input suggestions. */
  referencesFor(name: string): string[] {
    return modelsStore.referencesFor(name);
  }

  /** Canonical model references the sandbox may pick from. */
  get allModelKeys(): string[] {
    return modelsStore.models.map((spec) => modelsStore.referenceOf(spec));
  }

  setActiveModel(model: string) {
    this.activeModel = model;
  }

  selectProvider(name: string) {
    this.selectedProviderName = name;
  }

  /** Creates or replaces one endpoint. It never changes the global default model. */
  async upsertProvider(req: UpsertProviderRequest): Promise<boolean> {
    this.actionError = null;
    this.pending = true;
    try {
      this.catalog = await api.upsertProvider(req);
      this.selectedProviderName = req.name;
      // The node fills the model catalog from the endpoint's own listing while saving, so the
      // catalog is re-read here rather than left showing the pre-save snapshot.
      await modelsStore.load();
      return true;
    } catch (e) {
      this.actionError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.pending = false;
    }
  }

  /**
   * Deletes one endpoint together with its catalog entries.
   *
   * When it served the global default model, the node clears that default too, so the model
   * catalog is re-read to show the honest state.
   */
  async deleteProvider(name: string): Promise<boolean> {
    this.actionError = null;
    this.pending = true;
    try {
      this.catalog = await api.deleteProvider({ name });
      if (this.selectedProviderName === name) {
        this.selectedProviderName = this.providers[0]?.name ?? '';
      }
      await modelsStore.load();
      return true;
    } catch (e) {
      this.actionError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.pending = false;
    }
  }

  /**
   * Probes one endpoint and records the result.
   *
   * The request names the provider, so the *server* supplies the stored credential: the browser
   * never has it. Anything typed into the form is sent as an override, which lets an operator test
   * an edit before saving it.
   */
  async testProvider(
    name: string,
    req: TestProviderRequest,
  ): Promise<TestProviderResponse | null> {
    this.testingProvider = name;
    try {
      const res = await api.testProvider({ ...req, provider: name });
      this.providerTestResults = { ...this.providerTestResults, [name]: res };
      return res;
    } catch (e) {
      const failure: TestProviderResponse = {
        status: 'error',
        latency_ms: 0,
        model: req.model ?? '',
        reply: null,
        error: e instanceof Error ? e.message : String(e),
      };
      this.providerTestResults = {
        ...this.providerTestResults,
        [name]: failure,
      };
      return failure;
    } finally {
      this.testingProvider = null;
    }
  }

  /** Reads the endpoint's model listing and stores it, reporting how many entries were written. */
  async discoverModels(
    provider: string,
  ): Promise<DiscoverModelsResponse | null> {
    return modelsStore.discover(provider, true);
  }

  async load() {
    this.loading = true;
    this.error = null;
    try {
      const [cat, sys] = await Promise.all([
        api.getProviders(),
        api.getSystemConfig(),
      ]);
      this.catalog = cat;
      this.systemConfig = sys;
      if (
        !this.selectedProviderName ||
        !cat.providers.some((p) => p.name === this.selectedProviderName)
      ) {
        this.selectedProviderName = cat.providers[0]?.name ?? '';
      }
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  async refresh() {
    await this.load();
    await modelsStore.load();
  }
}

export const providersStore = new ProvidersStore();
