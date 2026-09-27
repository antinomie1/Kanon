import { api } from '../api/client';
import type {
  AdapterItem,
  BotInstanceView,
  InstanceRequest,
  InstancesResponse,
  ItemPolicy,
  PersonaItem,
  ReplyMode,
  ReplyPolicy,
} from '../types';
import { modelsStore } from './models.svelte';

/** One toggleable item an instance may opt in or out of. */
export interface PolicyItem {
  id: string;
  name: string;
}

/** Which policy map an override belongs to. */
export type PolicyKind = 'plugins' | 'skills' | 'mcp';

/**
 * Reply-policy selection offered by the instance form.
 *
 * `inherit` is not a server mode: it maps to `reply_policy: null`, which is how an instance says
 * "follow the node-wide policy".
 */
export type ReplyPolicyChoice = 'inherit' | ReplyMode;

/**
 * Console state for bot instances.
 *
 * Instances are a node-level catalog: adapters only declare where messages come from, while an
 * instance decides whether a bot answers them at all. This store keeps that catalog, the adapter
 * and persona choices it can be built from, and the form state shared by the create/edit modal.
 */
class InstancesStore {
  catalog = $state<InstancesResponse | null>(null);
  adapters = $state<AdapterItem[]>([]);
  personas = $state<PersonaItem[]>([]);
  // Toggleable items an instance may override. Loaded with the catalog so the form can render one
  // row per item without a second round trip.
  pluginItems = $state<PolicyItem[]>([]);
  skillItems = $state<PolicyItem[]>([]);
  mcpItems = $state<PolicyItem[]>([]);

  loading = $state(false);
  saving = $state(false);
  error = $state<string | null>(null);
  notice = $state<string | null>(null);

  /** Instance currently being edited, or `null` when the form creates a new one. */
  editingId = $state<string | null>(null);
  isFormOpen = $state(false);

  // Form fields
  formName = $state('');
  formEnabled = $state(true);
  formAdapters = $state<string[]>([]);
  formPersonaId = $state('');
  formSystemPrompt = $state('');
  /** Canonical catalog reference; empty means the instance inherits the node default model. */
  formModel = $state('');
  /** `inherit` sends `reply_policy: null`. */
  formReplyPolicyMode = $state<ReplyPolicyChoice>('inherit');
  /** Only consulted by the `probability` mode. */
  formReplyProbability = $state(0.5);
  formPlugins = $state<Record<string, ItemPolicy>>({});
  formSkills = $state<Record<string, ItemPolicy>>({});
  formMcp = $state<Record<string, ItemPolicy>>({});

  get instances(): BotInstanceView[] {
    return this.catalog?.instances ?? [];
  }

  get enabledCount(): number {
    return this.catalog?.enabled ?? 0;
  }

  /** Node-wide policy an instance without an override inherits, for the form hint. */
  get nodeReplyPolicy(): ReplyPolicy | null {
    return this.catalog?.node_reply_policy ?? null;
  }

  /** Canonical model references the operator may assign, seeded from the node's catalog. */
  get modelReferences(): string[] {
    return modelsStore.models.map((spec) => modelsStore.referenceOf(spec));
  }

  /** Node default model shown by the "inherit" option, when one is configured. */
  get nodeDefaultModel(): string | null {
    return modelsStore.defaultModel;
  }

  /** Items of one policy kind, in the order the console renders them. */
  itemsOf(kind: PolicyKind): PolicyItem[] {
    switch (kind) {
      case 'plugins':
        return this.pluginItems;
      case 'skills':
        return this.skillItems;
      case 'mcp':
        return this.mcpItems;
    }
  }

  /** Overrides currently drafted for one policy kind. */
  private draftOf(kind: PolicyKind): Record<string, ItemPolicy> {
    switch (kind) {
      case 'plugins':
        return this.formPlugins;
      case 'skills':
        return this.formSkills;
      case 'mcp':
        return this.formMcp;
    }
  }

  /** Resolved policy of one item in the open form. */
  policyOf(kind: PolicyKind, id: string): ItemPolicy {
    return this.draftOf(kind)[id] ?? 'inherit';
  }

  /**
   * Records one override.
   *
   * `inherit` is deleted rather than stored: the server treats an absent key as inherit, and
   * keeping it would grow the payload with entries that mean nothing.
   */
  setPolicy(kind: PolicyKind, id: string, policy: ItemPolicy) {
    const draft = { ...this.draftOf(kind) };
    if (policy === 'inherit') {
      delete draft[id];
    } else {
      draft[id] = policy;
    }
    switch (kind) {
      case 'plugins':
        this.formPlugins = draft;
        break;
      case 'skills':
        this.formSkills = draft;
        break;
      case 'mcp':
        this.formMcp = draft;
        break;
    }
  }

  /** Number of explicit overrides an instance carries, for the list summary. */
  overrideCount(instance: BotInstanceView): number {
    return (
      Object.keys(instance.plugins).length +
      Object.keys(instance.skills).length +
      Object.keys(instance.mcp).length
    );
  }

  /** Name of the instance that currently owns an adapter, if any. */
  ownerOf(platform: string): string | null {
    for (const instance of this.instances) {
      if (!instance.enabled || instance.id === this.editingId) continue;
      if (instance.adapters.includes(platform)) return instance.name;
    }
    return null;
  }

  async load() {
    this.loading = true;
    this.error = null;
    try {
      const [catalog, adapters, personas, plugins, skills, mcp] =
        await Promise.all([
          api.getInstances(),
          api.getAdapters(),
          api.getPersonas(),
          api.getPlugins(),
          api.getSkills(),
          api.getMcpServers(),
        ]);
      this.catalog = catalog;
      this.adapters = adapters.adapters;
      this.personas = personas.personas;
      this.pluginItems = plugins.plugins.map((plugin) => ({
        id: plugin.id,
        name: plugin.name || plugin.id,
      }));
      this.skillItems = skills.skills.map((skill) => ({
        id: skill.id,
        name: skill.name,
      }));
      this.mcpItems = mcp.servers.map((server) => ({
        id: server.id,
        name: server.name,
      }));
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.loading = false;
    }
  }

  openCreate() {
    this.editingId = null;
    this.formName = '';
    this.formEnabled = true;
    this.formAdapters = [];
    this.formPersonaId = '';
    this.formSystemPrompt = '';
    this.formModel = '';
    this.formReplyPolicyMode = 'inherit';
    this.formReplyProbability = 0.5;
    this.formPlugins = {};
    this.formSkills = {};
    this.formMcp = {};
    this.notice = null;
    this.error = null;
    this.isFormOpen = true;
  }

  openEdit(instance: BotInstanceView) {
    this.editingId = instance.id;
    this.formName = instance.name;
    this.formEnabled = instance.enabled;
    this.formAdapters = [...instance.adapters];
    this.formPersonaId = instance.persona_id ?? '';
    this.formSystemPrompt = instance.system_prompt ?? '';
    this.formModel = instance.model ?? '';
    // A null override is the `inherit` choice; any stored policy is shown verbatim.
    this.formReplyPolicyMode = instance.reply_policy?.mode ?? 'inherit';
    this.formReplyProbability = instance.reply_policy?.probability ?? 0.5;
    this.formPlugins = { ...instance.plugins };
    this.formSkills = { ...instance.skills };
    this.formMcp = { ...instance.mcp };
    this.notice = null;
    this.error = null;
    this.isFormOpen = true;
  }

  closeForm() {
    this.isFormOpen = false;
  }

  toggleAdapter(platform: string) {
    this.formAdapters = this.formAdapters.includes(platform)
      ? this.formAdapters.filter((p) => p !== platform)
      : [...this.formAdapters, platform];
  }

  private payload(): InstanceRequest {
    return {
      name: this.formName.trim(),
      enabled: this.formEnabled,
      adapters: this.formAdapters,
      persona_id: this.formPersonaId || null,
      system_prompt: this.formSystemPrompt.trim() || null,
      model: this.formModel.trim() || null,
      reply_policy:
        this.formReplyPolicyMode === 'inherit'
          ? null
          : {
              mode: this.formReplyPolicyMode,
              probability: this.formReplyProbability,
            },
      plugins: this.formPlugins,
      skills: this.formSkills,
      mcp: this.formMcp,
    };
  }

  async save() {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      const body = this.payload();
      const res = this.editingId
        ? await api.updateInstance(this.editingId, body)
        : await api.createInstance(body);
      this.notice = res.message;
      this.isFormOpen = false;
      await this.load();
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }

  async toggleEnabled(instance: BotInstanceView) {
    this.saving = true;
    this.error = null;
    try {
      const res = await api.updateInstance(instance.id, {
        name: instance.name,
        enabled: !instance.enabled,
        adapters: instance.adapters,
        persona_id: instance.persona_id,
        system_prompt: instance.system_prompt,
        model: instance.model,
        // Preserved verbatim: a start/stop toggle must not silently drop the instance's overrides.
        reply_policy: instance.reply_policy,
        plugins: instance.plugins,
        skills: instance.skills,
        mcp: instance.mcp,
      });
      this.notice = res.message;
      await this.load();
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.saving = false;
    }
  }

  async remove(id: string) {
    this.saving = true;
    this.error = null;
    try {
      const res = await api.deleteInstance(id);
      this.notice = res.message;
      await this.load();
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
    } finally {
      this.saving = false;
    }
  }
}

export const instancesStore = new InstancesStore();
