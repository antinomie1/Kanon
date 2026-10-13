import type { House } from 'lucide-svelte';

/** Any lucide icon component; every icon is generated with the same component type. */
export type IconComponent = typeof House;

// Node health & metrics types
export interface MemorySection {
  resident_bytes: number | null;
  virtual_bytes: number | null;
}

export interface PluginSection {
  hosts: number;
  loaded: number;
}

export interface SessionSection {
  total: number;
  active: number;
}

export interface RealtimeSection {
  websocket_connections: number;
  log_subscribers: number;
  event_subscribers: number;
}

export interface InstanceSection {
  total: number;
  enabled: number;
}

export interface NodeHealth {
  /** Bot instance gate: an adapter answers nothing until an instance claims it. */
  instances: InstanceSection;
  status: string;
  version: string;
  uptime_seconds: number;
  llm_configured: boolean;
  memory: MemorySection;
  plugins: PluginSection;
  sessions: SessionSection;
  realtime: RealtimeSection;
}

// System configuration types
export interface LlmConfig {
  /** Whether the node has a model to answer with. */
  configured: boolean;
  /** Canonical `<provider>/<model-id>` the node answers with by default; empty when unset. */
  model: string;
  /** Provider serving that model. */
  provider: string | null;
  context_length: number | null;
  max_iterations: number;
  temperature: number | null;
  max_tokens: number | null;
}

export interface EnvironmentConfig {
  os: string;
  arch: string;
  rust_edition: string;
}

export interface SystemConfig {
  version: string;
  uptime_seconds: number;
  ipc_socket_path: string;
  run_dir: string;
  data_dir: string;
  llm: LlmConfig;
  /** Node-wide reply policy inherited by instances without an override. */
  reply_policy: ReplyPolicy;
  /** Node-wide context-extras policy inherited by instances without an override. */
  context_policy: ContextPolicy;
  environment: EnvironmentConfig;
}

// Node-wide reply policy
/** How an instance or the node decides whether to answer a group conversation. */
export type ReplyMode = 'always' | 'mention' | 'probability' | 'never';

/**
 * Reply policy of an instance or of the node as a whole.
 *
 * `probability` is only consulted by the `probability` mode and must stay within `0..=1`; the
 * server rejects values outside that range rather than clamping them, so a typo is visible.
 */
export interface ReplyPolicy {
  mode: ReplyMode;
  probability: number;
  /** Quote the message being answered in a group or channel (never in private chats). */
  quote_message: boolean;
  /** Show progress feedback (typing, a reaction) before the model answers. */
  acknowledge: boolean;
  /** Send each nonblank line of a model answer as a separate platform message. */
  split_lines: boolean;
  /** Send the model's reasoning, as plain text, ahead of its answer. */
  send_reasoning: boolean;
}

/**
 * Whether identifying or contextual extras are prepended to the model prompt.
 *
 * Both default to off: a sender id is personal data and a wall-clock time is not part of what the
 * user said, so including either is an explicit operator decision.
 */
export interface ContextPolicy {
  /** Include the conversation id (group number, channel id, …) in the prompt. */
  include_channel_id: boolean;
  /** Include the platform sender id (QQ number, openid, …) in the prompt. */
  include_sender_id: boolean;
  /** Include the message timestamp in the prompt. */
  include_timestamp: boolean;
  /** Expand a merged forward into the messages it carries (on by default). */
  expand_forward: boolean;
}

/**
 * Which platform notices the bot reacts to, node-wide.
 *
 * Reactions are off by default; recall notes are on because they only correct what the model
 * already saw and are never visible in the chat.
 */
export interface EventPolicy {
  /** Welcome a member who joined a group. */
  welcome_members: boolean;
  /** Say hello when the bot is added to a group or as a friend. */
  greet_on_join: boolean;
  /** Answer when somebody pokes the bot. */
  reply_to_poke: boolean;
  /** Tell the model when a message it saw was recalled. */
  note_recalls: boolean;
  /** Accept friend requests automatically. */
  accept_friend_requests: boolean;
  /** Accept group invitations automatically. */
  accept_group_invites: boolean;
}

/** Who may run a command. */
export type CommandAccess = 'everyone' | 'admins_in_groups' | 'admins';

/** Command permissions and bot administrators, node-wide or as an instance override. */
export interface CommandPolicy {
  /** Administrators as `<platform>:<user id>`. */
  admins: string[];
  /** Treat group owners and admins as bot administrators in their group. */
  group_admins_are_admins: boolean;
  /** Access level per command name (without the slash); unlisted commands are open. */
  access: Record<string, CommandAccess>;
}

/** Response of `GET`/`PUT /api/v1/system/command-policy`. */
export interface CommandPolicyResponse {
  policy: CommandPolicy;
}

/** Whether group members each have a session with the bot or share one. */
export type SessionScope = 'user' | 'group';

/**
 * Where an instance lets its administrators run Bash; the node-wide Bash switch still wins.
 *
 * `own_context` covers private chats and per-member group sessions of a group the instance does
 * not observe. `shared_context` also allows shared or observed group sessions, whose other
 * members' words can steer the commands the model runs.
 */
export type BashScope = 'disabled' | 'own_context' | 'shared_context';

/** Response of `GET`/`PUT /api/v1/system/event-policy`. */
export interface EventPolicyResponse {
  policy: EventPolicy;
}

/** Response of `GET`/`PUT /api/v1/system/context-policy`. */
export interface ContextPolicyResponse {
  policy: ContextPolicy;
}

/** Response of `GET`/`PUT /api/v1/system/reply-policy`. */
export interface ReplyPolicyResponse {
  policy: ReplyPolicy;
  /** Human-readable rendering of the policy, produced by the node. */
  description: string;
}

// Bot instance types
export interface AdapterStatus {
  platform: string;
  /** Whether the node knows this platform at all (catches typos). */
  known: boolean;
  connected: boolean;
  display_name: string | null;
  kind: 'builtin' | 'plugin' | null;
}

/**
 * Per-instance override for a toggleable item (plugin, skill or MCP server).
 *
 * `inherit` follows the node-wide switch, `enable` documents an explicit opt-in (the global switch
 * still wins), and `disable` never uses the item for this instance.
 */
export type ItemPolicy = 'inherit' | 'enable' | 'disable';

/** Participation behavior, independent of model and persona. */
export type ConversationMode = 'assistant' | 'simulation';

/** Bounded observation, listening and speech settings. */
export interface SimulationPolicy {
  quiet_ms: number;
  max_batch_ms: number;
  listen_seconds: number;
  max_participation_seconds: number;
  max_messages: number;
}

export interface BotInstanceView {
  id: string;
  name: string;
  enabled: boolean;
  conversation_mode?: ConversationMode;
  conversation_rules?: boolean;
  simulation?: SimulationPolicy;
  adapters: string[];
  persona_id: string | null;
  system_prompt: string | null;
  /** Agent override; `null` inherits the node's default agent. */
  agent: string | null;
  model: string | null;
  /** Reply-policy override; `null` inherits the node-wide policy. */
  reply_policy: ReplyPolicy | null;
  /** Context-extras override; `null` inherits the node-wide policy. */
  context_policy: ContextPolicy | null;
  session_scope: SessionScope;
  observe_group: boolean;
  /** Command-permission override; `null` inherits the node-wide policy. */
  command_policy: CommandPolicy | null;
  bash: BashScope;
  plugins: Record<string, ItemPolicy>;
  skills: Record<string, ItemPolicy>;
  mcp: Record<string, ItemPolicy>;
  adapter_status: AdapterStatus[];
}

export interface InstancesResponse {
  total: number;
  enabled: number;
  /** Node-wide reply policy inherited by instances without an override. */
  node_reply_policy: ReplyPolicy;
  /** Node-wide context-extras policy inherited by instances without an override. */
  node_context_policy: ContextPolicy;
  /** Node-wide command policy inherited by instances without an override. */
  node_command_policy: CommandPolicy;
  /** Whether Bash is switched on node-wide; an instance scope cannot enable it on its own. */
  node_bash_enabled: boolean;
  instances: BotInstanceView[];
}

export interface InstanceRequest {
  name: string;
  enabled: boolean;
  conversation_mode?: ConversationMode;
  conversation_rules?: boolean;
  simulation?: SimulationPolicy;
  adapters: string[];
  persona_id?: string | null;
  system_prompt?: string | null;
  /** `null` (or omitted) inherits the node's default agent. */
  agent?: string | null;
  model?: string | null;
  /** `null` (or omitted) inherits the node-wide reply policy. */
  reply_policy?: ReplyPolicy | null;
  /** `null` (or omitted) inherits the node-wide context policy. */
  context_policy?: ContextPolicy | null;
  session_scope?: SessionScope;
  observe_group?: boolean;
  /** `null` (or omitted) inherits the node-wide command policy. */
  command_policy?: CommandPolicy | null;
  bash?: BashScope;
  plugins?: Record<string, ItemPolicy>;
  skills?: Record<string, ItemPolicy>;
  mcp?: Record<string, ItemPolicy>;
}

export interface InstanceMutationResponse {
  applied: boolean;
  message: string;
  instance: BotInstanceView | null;
}

// Provider & Models types
/** Input modalities and behaviours a model advertises. */
export interface ModelCapabilities {
  text: boolean;
  vision: boolean;
  audio: boolean;
  video: boolean;
  tool_calling: boolean;
  reasoning: boolean;
}

/** Provenance of a model's settings, so the console can explain where a value came from. */
export type ModelSettingsSource = 'unknown' | 'upstream' | 'manual';

/**
 * One catalog entry, addressed as `<provider>/<model>`.
 *
 * Context window and modalities belong to a model *as served by an endpoint*, which is why the
 * catalog is keyed by the full reference instead of the bare model id.
 */
export interface ModelSpec {
  provider: string;
  model: string;
  display_name?: string;
  context_length?: number;
  max_output_tokens?: number;
  capabilities: ModelCapabilities;
  temperature?: number;
  source?: ModelSettingsSource;
}

/** Response of `GET /api/v1/agents` and `PUT /api/v1/agents/default`. */
export interface AgentsResponse {
  /** Agents an operator may select, node-wide or per instance (only `builtin` today). */
  agents: string[];
  /** Agent that answers for every instance without an override. */
  default_agent: string;
}

/** Response of `GET /api/v1/models` and every model mutation. */
export interface ModelsResponse {
  models: ModelSpec[];
  total: number;
  default_model: string | null;
  /** Provider names a model reference may use. */
  providers: string[];
}

/** Request body of `POST /api/v1/models/delete`. */
export interface DeleteModelRequest {
  /** Canonical `<provider>/<model-id>` reference to remove. */
  reference: string;
}

/** Request body of `POST /api/v1/models/discover`. */
export interface DiscoverModelsRequest {
  provider: string;
  /** When true the discovered entries are also stored in the catalog. */
  persist: boolean;
}

/** Response of `POST /api/v1/models/discover`. */
export interface DiscoverModelsResponse {
  provider: string;
  discovered: ModelSpec[];
  /** Entries added or refreshed; zero when `persist` was false. */
  persisted: number;
}

/** One configured named endpoint, credential excluded. */
export interface ProviderInfo {
  name: string;
  protocol: string;
  base_url: string;
  api_key_configured: boolean;
  temperature: number | null;
  max_tokens: number | null;
  replay_reasoning: boolean;
}

export interface ProtocolDescriptor {
  id: string;
  name: string;
  default_base_url: string;
}

export interface ProviderPreset {
  id: string;
  name: string;
  protocol: string;
  base_url: string;
}

export interface ProvidersCatalog {
  /** Every configured provider endpoint. Providers are endpoints only: none of them is "the default". */
  providers: ProviderInfo[];
  available_protocols: ProtocolDescriptor[];
  presets: ProviderPreset[];
}

/** Request body of `POST /api/v1/providers` (create or replace one named endpoint). */
export interface UpsertProviderRequest {
  name: string;
  protocol: string;
  base_url: string;
  /** Omitted keeps the stored credential. */
  api_key?: string;
  /** Explicitly removes the stored credential. */
  clear_api_key?: boolean;
  temperature?: number;
  max_tokens?: number;
  replay_reasoning?: boolean;
}

/** Request body of `PUT /api/v1/models/default`; `null` clears the global default model. */
export interface SetDefaultModelRequest {
  model: string | null;
}

/** Request body of `POST /api/v1/providers/delete`. */
export interface DeleteProviderRequest {
  name: string;
}

/**
 * Request body of `POST /api/v1/providers/test`.
 *
 * `provider` names a configured endpoint, whose stored credential the server uses; the other
 * fields are optional overrides for a form that is edited but not saved yet. Without `provider`,
 * `protocol` and `base_url` describe a throw-away endpoint.
 */
export interface TestProviderRequest {
  provider?: string;
  protocol?: string;
  base_url?: string;
  api_key?: string;
  /** Probe without a credential, matching the editor's remove-key choice. */
  clear_api_key?: boolean;
  /** Upstream model id exactly as the endpoint expects it (no provider prefix). */
  model?: string;
  prompt?: string;
}

export interface TestProviderResponse {
  status: 'ok' | 'error';
  latency_ms: number;
  model: string;
  reply: string | null;
  error: string | null;
}

/** Request body of `POST /api/v1/providers/models`. */
export interface FetchModelsRequest {
  protocol?: string;
  base_url: string;
  api_key?: string;
  /** Provider name used for discovery; the server derives one from the URL when omitted. */
  provider?: string;
}

/** Response of `POST /api/v1/providers/models`. */
export interface FetchModelsResponse {
  /** Flat identifier list older clients expect. */
  models: string[];
  /** Full catalog candidates carrying the metadata the endpoint reported. */
  candidates: ModelSpec[];
}


export * from "./extensions";
export * from "./adapters";
export * from "./sessions";
