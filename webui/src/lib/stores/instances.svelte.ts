import { api } from '../api/client';
import type {
  AdapterItem,
  BashScope,
  BotInstanceView,
  CommandPolicy,
  ContextPolicy,
  InstanceRequest,
  InstancesResponse,
  ItemPolicy,
  PersonaItem,
  ReplyMode,
  ReplyPolicy,
  SessionScope,
} from '../types';
import {
  type CommandPolicyDraft,
  commandPolicyOfDraft,
  draftOfCommandPolicy,
} from './commandPolicy.svelte';
import { modelsStore } from './models.svelte';

/** One toggleable item an instance may opt in or out of. */
export interface PolicyItem {
  id: string;
  name: string;
}

/** A platform that an enabled instance answers on but that cannot deliver messages right now. */
export interface AdapterProblem {
  instanceId: string;
  instanceName: string;
  platform: string;
  /** Adapter name as the node reports it, falling back to the platform id. */
  displayName: string;
  /** `unknown` means no adapter on the node registers this platform (usually a typo or a removed plugin). */
  reason: 'disconnected' | 'unknown';
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
 * and persona choices it can be built from, and the draft of the instance open in the editor.
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
  /**
   * The request the open form would have sent when it was opened. Comparing the live payload with
   * it is what tells the editor whether (and how much) the operator changed, so the save bar only
   * appears when there is something to save.
   */
  private baseline = $state.raw<InstanceRequest | null>(null);

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
  /** `true` sends `context_policy: null`, inheriting the node-wide context policy. */
  formContextInherit = $state(true);
  formIncludeChannelId = $state(false);
  formIncludeSenderId = $state(false);
  formIncludeTimestamp = $state(false);
  /** Quote the answered message in groups (reply override only). */
  formReplyQuote = $state(false);
  /** Progress feedback before answering (reply override only). */
  formReplyAck = $state(false);
  /** Send nonblank answer lines separately (reply override only). */
  formReplySplitLines = $state(false);
  /** Expand merged forwards (context override only). */
  formExpandForward = $state(true);
  /** Per-member or shared group sessions. */
  formSessionScope = $state<SessionScope>('user');
  /** Show unanswered group messages to the model on its next turn. */
  formObserveGroup = $state(false);
  /** `true` sends `command_policy: null`, inheriting the node-wide command policy. */
  formCommandInherit = $state(true);
  /** Command-policy override being edited; only sent when not inheriting. */
  formCommandDraft = $state<CommandPolicyDraft>({
    admins: '',
    groupAdminsAreAdmins: true,
    rows: [],
  });
  /** Where this instance's administrators may run Bash. */
  formBash = $state<BashScope>('own_context');
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

  /** Node-wide context policy an instance without an override inherits, for the form hint. */
  get nodeContextPolicy(): ContextPolicy | null {
    return this.catalog?.node_context_policy ?? null;
  }

  /** Node-wide command policy an instance without an override inherits. */
  get nodeCommandPolicy(): CommandPolicy | null {
    return this.catalog?.node_command_policy ?? null;
  }

  /** Whether Bash is switched on node-wide; an instance scope cannot enable it on its own. */
  get nodeBashEnabled(): boolean {
    return this.catalog?.node_bash_enabled ?? false;
  }

  /** Node default model shown by the "inherit" option, when one is configured. */
  get nodeDefaultModel(): string | null {
    return modelsStore.defaultModel;
  }

  /**
   * Platforms enabled instances rely on that are not delivering messages.
   *
   * Only claimed platforms count: an unused adapter that is offline affects nobody, while a claimed
   * one silently drops every message for that instance, which is exactly what needs attention.
   */
  get adapterProblems(): AdapterProblem[] {
    const problems: AdapterProblem[] = [];
    for (const instance of this.instances) {
      if (!instance.enabled) continue;
      for (const status of instance.adapter_status) {
        if (status.known && status.connected) continue;
        problems.push({
          instanceId: instance.id,
          instanceName: instance.name,
          platform: status.platform,
          displayName: status.display_name || status.platform,
          reason: status.known ? 'disconnected' : 'unknown',
        });
      }
    }
    return problems;
  }

  /** Number of top-level settings the open form changed; zero when there is nothing to save. */
  get changeCount(): number {
    const base = this.baseline;
    if (!base || !this.isFormOpen) return 0;
    const now = this.payload();
    let count = 0;
    for (const key of Object.keys(now) as (keyof InstanceRequest)[]) {
      if (
        JSON.stringify(now[key] ?? null) !== JSON.stringify(base[key] ?? null)
      ) {
        count++;
      }
    }
    return count;
  }

  /** Model the instance answers with, and whether that is the node default rather than its own. */
  effectiveModel(instance: BotInstanceView): {
    reference: string | null;
    inherited: boolean;
  } {
    if (instance.model) return { reference: instance.model, inherited: false };
    return { reference: this.nodeDefaultModel, inherited: true };
  }

  /** Reply policy the instance follows in groups, and whether it is the node-wide one. */
  effectiveReplyPolicy(instance: BotInstanceView): {
    policy: ReplyPolicy | null;
    inherited: boolean;
  } {
    if (instance.reply_policy)
      return { policy: instance.reply_policy, inherited: false };
    return { policy: this.nodeReplyPolicy, inherited: true };
  }

  /** The instance with `id`, if the catalog has it. */
  find(id: string | null): BotInstanceView | null {
    if (!id) return null;
    return this.instances.find((instance) => instance.id === id) ?? null;
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

  /**
   * Refreshes the catalog and adapter connection state only.
   *
   * Polled by the shell so connection problems appear and clear without a page reload; it leaves
   * the form and the catalogs of personas, plugins, skills and MCP servers alone.
   */
  async refreshStatus() {
    try {
      const [catalog, adapters] = await Promise.all([
        api.getInstances(),
        api.getAdapters(),
      ]);
      this.catalog = catalog;
      this.adapters = adapters.adapters;
    } catch {
      // The node banner already reports an unreachable node; a failed poll keeps the last state.
    }
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
      // Instance personas are generated from an instance's own prompt; only the base assistant
      // and the operator's library are offered as a choice.
      this.personas = personas.personas.filter(
        (persona) => persona.kind !== 'instance',
      );
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
    this.formReplyQuote = false;
    this.formReplyAck = false;
    this.formReplySplitLines = false;
    this.formContextInherit = true;
    this.formExpandForward = true;
    this.formSessionScope = 'user';
    this.formObserveGroup = false;
    this.formCommandInherit = true;
    this.formCommandDraft = this.inheritedCommandDraft();
    this.formBash = 'own_context';
    this.formIncludeChannelId = false;
    this.formIncludeSenderId = false;
    this.formIncludeTimestamp = false;
    this.formPlugins = {};
    this.formSkills = {};
    this.formMcp = {};
    this.notice = null;
    this.error = null;
    this.isFormOpen = true;
    this.baseline = this.payload();
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
    this.formReplyQuote = instance.reply_policy?.quote_message ?? false;
    this.formReplyAck = instance.reply_policy?.acknowledge ?? false;
    this.formReplySplitLines = instance.reply_policy?.split_lines ?? false;
    // A null override is the `inherit` choice; a stored policy is shown verbatim.
    this.formContextInherit = instance.context_policy === null;
    this.formIncludeChannelId =
      instance.context_policy?.include_channel_id ?? false;
    this.formIncludeSenderId =
      instance.context_policy?.include_sender_id ?? false;
    this.formExpandForward = instance.context_policy?.expand_forward ?? true;
    this.formSessionScope = instance.session_scope ?? 'user';
    this.formObserveGroup = instance.observe_group ?? false;
    // A null override is the `inherit` choice; the editor then starts from the node policy, so
    // switching to an override begins with what currently applies instead of an empty list.
    this.formCommandInherit = instance.command_policy === null;
    this.formCommandDraft = instance.command_policy
      ? draftOfCommandPolicy(instance.command_policy)
      : this.inheritedCommandDraft();
    this.formBash = instance.bash ?? 'own_context';
    this.formIncludeTimestamp =
      instance.context_policy?.include_timestamp ?? false;
    this.formPlugins = { ...instance.plugins };
    this.formSkills = { ...instance.skills };
    this.formMcp = { ...instance.mcp };
    this.notice = null;
    this.error = null;
    this.isFormOpen = true;
    this.baseline = this.payload();
  }

  /** Puts the form back to what it was when it was opened. */
  discardChanges() {
    if (this.editingId) {
      const instance = this.find(this.editingId);
      if (instance) this.openEdit(instance);
    } else {
      this.openCreate();
    }
  }

  closeForm() {
    this.isFormOpen = false;
    this.editingId = null;
    this.baseline = null;
  }

  /** Editor seed for an instance that inherits: a copy of the node-wide command policy. */
  private inheritedCommandDraft(): CommandPolicyDraft {
    const node = this.nodeCommandPolicy;
    return node
      ? draftOfCommandPolicy(node)
      : { admins: '', groupAdminsAreAdmins: true, rows: [] };
  }

  toggleAdapter(platform: string) {
    this.formAdapters = this.formAdapters.includes(platform)
      ? this.formAdapters.filter((p) => p !== platform)
      : [...this.formAdapters, platform];
  }

  payload(): InstanceRequest {
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
              quote_message: this.formReplyQuote,
              acknowledge: this.formReplyAck,
              split_lines: this.formReplySplitLines,
            },
      context_policy: this.formContextInherit
        ? null
        : {
            include_channel_id: this.formIncludeChannelId,
            include_sender_id: this.formIncludeSenderId,
            include_timestamp: this.formIncludeTimestamp,
            expand_forward: this.formExpandForward,
          },
      session_scope: this.formSessionScope,
      observe_group: this.formObserveGroup,
      command_policy: this.formCommandInherit
        ? null
        : commandPolicyOfDraft(this.formCommandDraft),
      bash: this.formBash,
      plugins: this.formPlugins,
      skills: this.formSkills,
      mcp: this.formMcp,
    };
  }

  /**
   * Creates or updates the instance in the form.
   *
   * Resolves to the saved instance id, or `null` when the node rejected it (the reason is in
   * `error` and the form keeps what was typed). After a save the form is reopened on the stored
   * instance, so what the editor shows is what the node now runs.
   */
  async save(): Promise<string | null> {
    this.saving = true;
    this.error = null;
    this.notice = null;
    try {
      const body = this.payload();
      const res = this.editingId
        ? await api.updateInstance(this.editingId, body)
        : await api.createInstance(body);
      this.notice = res.message;
      await this.load();
      const saved = this.find(res.instance?.id ?? this.editingId);
      if (saved) {
        this.openEdit(saved);
      } else {
        this.closeForm();
      }
      return saved?.id ?? null;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return null;
    } finally {
      this.saving = false;
    }
  }

  /**
   * Starts or stops an instance right away, outside the form.
   *
   * Resolves to `true` when the node applied it. A draft open on the same instance keeps its other
   * edits; only its on/off state (and the baseline it is compared with) follows the node.
   */
  async toggleEnabled(instance: BotInstanceView): Promise<boolean> {
    this.saving = true;
    this.error = null;
    const enabled = !instance.enabled;
    try {
      // Every other field is sent back verbatim: an update replaces the whole instance, so a
      // start/stop toggle must not silently reset its overrides to their defaults.
      const res = await api.updateInstance(instance.id, {
        name: instance.name,
        enabled,
        adapters: instance.adapters,
        persona_id: instance.persona_id,
        system_prompt: instance.system_prompt,
        model: instance.model,
        reply_policy: instance.reply_policy,
        context_policy: instance.context_policy,
        session_scope: instance.session_scope,
        observe_group: instance.observe_group,
        command_policy: instance.command_policy,
        bash: instance.bash,
        plugins: instance.plugins,
        skills: instance.skills,
        mcp: instance.mcp,
      });
      this.notice = res.message;
      if (this.isFormOpen && this.editingId === instance.id) {
        this.formEnabled = enabled;
        if (this.baseline) this.baseline = { ...this.baseline, enabled };
      }
      await this.refreshStatus();
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }

  /** Deletes an instance; resolves to `true` when the node removed it. */
  async remove(id: string): Promise<boolean> {
    this.saving = true;
    this.error = null;
    try {
      const res = await api.deleteInstance(id);
      this.notice = res.message;
      if (this.editingId === id) this.closeForm();
      await this.load();
      return true;
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e);
      return false;
    } finally {
      this.saving = false;
    }
  }
}

export const instancesStore = new InstancesStore();
