import { api } from '../api/client';
import type {
  ActivateProviderRequest,
  ActivateProviderResponse,
  ActiveProviderInfo,
  DiscoverModelsResponse,
  ProviderInfo,
  ProvidersCatalog,
  SetDefaultProviderRequest,
  SystemConfig,
  TestProviderRequest,
  TestProviderResponse,
  UpsertProviderRequest,
} from '../types';
import { modelsStore } from './models.svelte';

/**
 * Console state for the node's LLM directory.
 *
 * # One source of truth
 * The named endpoints live in the node's `data/system.json`; the browser keeps no copy. Every
 * mutation posts to the gateway and replaces the whole catalog with the response, so the console
 * can never describe a directory the running node does not have. The stored credential is never
 * returned — only whether one exists — which is why editing a provider leaves the key field blank
 * and omitting it keeps whatever the node already holds.
 */
class ProvidersStore {
  catalog = $state<ProvidersCatalog | null>(null);
  systemConfig = $state<SystemConfig | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);

  /**
   * The provider the *node* is actually using, as reported by the backend.
   *
   * This is the only value that decides whether the bot answers messages; the catalog's default
   * provider is the persisted intention, which the runtime snapshot confirms.
   */
  nodeProvider = $state<ActiveProviderInfo | null>(null);
  nodeActionPending = $state(false);
  nodeMessage = $state<string | null>(null);
  nodeError = $state<string | null>(null);

  /** Name of the endpoint currently open in the editor. */
  selectedProviderName = $state<string>('');

  /** Connectivity test result per provider name. */
  providerTestResults = $state<Record<string, TestProviderResponse>>({});
  testingProvider = $state<string | null>(null);

  constructor() {
    this.load();
  }

  get providers(): ProviderInfo[] {
    return this.catalog?.providers ?? [];
  }

  /** Endpoint used for unprefixed model references, when one is configured. */
  get defaultProvider(): string | null {
    return this.catalog?.default_provider ?? null;
  }

  /** Canonical model reference the node answers with by default. */
  get defaultModel(): string | null {
    return this.catalog?.default_model ?? null;
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

  /**
   * Canonical model references the sandbox may pick from, node default first.
   *
   * The catalog is the source of truth; the node's effective model is still listed when the catalog
   * is empty so a freshly configured endpoint stays usable from the playground.
   */
  get allModelKeys(): string[] {
    const keys = modelsStore.models.map((spec) =>
      modelsStore.referenceOf(spec),
    );
    const fallback = this.defaultModel ?? this.nodeProvider?.model;
    if (fallback && !keys.includes(fallback)) {
      keys.unshift(fallback);
    }
    return keys;
  }

  /** Sandbox model choice; empty means "let the node decide". */
  activeModel = $state<string>('');

  setActiveModel(model: string) {
    this.activeModel = model;
  }

  selectProvider(name: string) {
    this.selectedProviderName = name;
  }

  /** Creates or replaces one endpoint; `make_default` also repoints the node's default model. */
  async upsertProvider(req: UpsertProviderRequest): Promise<boolean> {
    this.nodeMessage = null;
    this.nodeError = null;
    this.nodeActionPending = true;
    try {
      this.catalog = await api.upsertProvider(req);
      this.selectedProviderName = req.name;
      // The node fills the model catalog from the endpoint's own listing while saving, so the
      // catalog is re-read here rather than left showing the pre-save snapshot.
      await modelsStore.load();
      return true;
    } catch (e) {
      this.nodeError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.nodeActionPending = false;
    }
  }

  async deleteProvider(name: string): Promise<boolean> {
    this.nodeMessage = null;
    this.nodeError = null;
    this.nodeActionPending = true;
    try {
      this.catalog = await api.deleteProvider({ name });
      if (this.selectedProviderName === name) {
        this.selectedProviderName = this.providers[0]?.name ?? '';
      }
      // Removing an endpoint removes its catalog entries too.
      await modelsStore.load();
      return true;
    } catch (e) {
      this.nodeError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.nodeActionPending = false;
    }
  }

  /**
   * Makes one endpoint the default.
   *
   * A model reference is mandatory when the current default model belongs to another endpoint:
   * the node refuses rather than sending one provider's model id to a different service.
   */
  async setDefaultProvider(req: SetDefaultProviderRequest): Promise<boolean> {
    this.nodeMessage = null;
    this.nodeError = null;
    this.nodeActionPending = true;
    try {
      this.catalog = await api.setDefaultProvider(req);
      return true;
    } catch (e) {
      this.nodeError = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.nodeActionPending = false;
    }
  }

  /**
   * Legacy single-endpoint activation, surfaced as "create default provider".
   *
   * It registers one endpoint from the values the operator just typed and makes it the default —
   * the path that works when no endpoint exists yet and the console therefore has no credential to
   * reuse.
   */
  async activateOnNode(
    req: ActivateProviderRequest,
  ): Promise<ActivateProviderResponse | null> {
    this.nodeMessage = null;
    this.nodeError = null;
    this.nodeActionPending = true;
    try {
      const res = await api.activateProvider(req);
      this.nodeProvider = res.active;
      // Activation registers or replaces a directory entry, so the catalog must be re-read; the
      // node also fills the model catalog from the endpoint's listing while activating.
      await this.load();
      await modelsStore.load();
      this.nodeMessage = res.message;
      return res;
    } catch (e) {
      this.nodeError = e instanceof Error ? e.message : String(e);
      return null;
    } finally {
      this.nodeActionPending = false;
    }
  }

  /** Clears every endpoint, disabling chat until one is applied again. */
  async clearOnNode(): Promise<ActivateProviderResponse | null> {
    this.nodeMessage = null;
    this.nodeError = null;
    this.nodeActionPending = true;
    try {
      const res = await api.clearActiveProvider();
      this.nodeProvider = res.active;
      await this.load();
      // Clearing every endpoint also clears the model catalog that described them.
      await modelsStore.load();
      this.nodeMessage = res.message;
      return res;
    } catch (e) {
      this.nodeError = e instanceof Error ? e.message : String(e);
      return null;
    } finally {
      this.nodeActionPending = false;
    }
  }

  /**
   * Probes one endpoint and records the result.
   *
   * The stored credential is never sent to the browser, so a probe of the endpoint the node is
   * already using omits the coordinates entirely and lets the server reuse its own stored entry;
   * any other endpoint is probed with what the form holds, because there is nothing else to use.
   */
  async testProvider(
    name: string,
    req: TestProviderRequest,
  ): Promise<TestProviderResponse | null> {
    this.testingProvider = name;
    try {
      const res = await api.testProvider(req);
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
      this.nodeProvider = cat.active;
      this.systemConfig = sys;
      if (
        !this.selectedProviderName ||
        !cat.providers.some((p) => p.name === this.selectedProviderName)
      ) {
        this.selectedProviderName = cat.providers[0]?.name ?? '';
      }
      if (!this.activeModel) {
        this.activeModel = cat.default_model ?? cat.active.model ?? '';
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

  async runTest(req?: TestProviderRequest) {
    return api.testProvider(req);
  }
}

export const providersStore = new ProvidersStore();
