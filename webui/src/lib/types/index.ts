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

export interface BotInstanceView {
  id: string;
  name: string;
  enabled: boolean;
  adapters: string[];
  persona_id: string | null;
  system_prompt: string | null;
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
  adapters: string[];
  persona_id?: string | null;
  system_prompt?: string | null;
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

// Plugin & Supervisor types
export interface CommandDescriptor {
  name: string;
  description: string;
  usage?: string;
}

export interface ToolDescriptor {
  name: string;
  description: string;
  parameters?: Record<string, unknown>;
}

export interface HostHealth {
  /** `running`, `restarting`, `crashed` or `disabled`. */
  state: 'running' | 'restarting' | 'crashed' | 'disabled' | string;
  /** Automatic restarts performed by the supervisor watchdog since node start. */
  restarts: number;
  last_error: string | null;
}

export interface PluginMeta {
  id: string;
  name: string;
  version: string;
  description?: string;
  status?: string;
  /** Whether the operator allows this plugin to run (disabling stops its host process). */
  enabled: boolean;
  /** Watchdog health, present when the plugin has a host process. */
  health?: HostHealth | null;
  commands: CommandDescriptor[];
  tools: ToolDescriptor[];
}

/** Where one callable tool comes from. */
export type ToolSource = 'builtin' | 'plugin' | 'mcp';

/** One tool the model can call, with the provider that exposes it. */
export interface ToolItem {
  /** Name the model must use when calling the tool. */
  name: string;
  description: string;
  source: ToolSource;
  /** Plugin id, MCP server id, or `kanon-core` for builtins. */
  provider_id: string;
  /** Host process exposing the tool; absent for builtins. */
  host_id?: string;
  /** JSON Schema of the accepted arguments. */
  parameters: Record<string, unknown>;
}

export interface ToolCatalog {
  total: number;
  builtin: number;
  plugin: number;
  mcp: number;
  tools: ToolItem[];
}

/** An installed skill and its node-wide switch. */
export interface SkillItem {
  id: string;
  name: string;
  description: string;
  enabled: boolean;
}

export interface SkillCatalog {
  skills: SkillItem[];
}

export interface SkillStateResponse {
  applied: boolean;
  message: string;
  skill_id: string;
  enabled: boolean;
}

/** How the node reaches one MCP server. */
export type McpTransport =
  | {
      type: 'stdio';
      command: string;
      args: string[];
      env: Record<string, string>;
    }
  | { type: 'http'; url: string; headers: Record<string, string> };

export interface McpHealth {
  /** `connecting`, `connected`, `reconnecting`, `failed` or `disabled`. */
  state: string;
  /** Tools the server currently advertises. */
  tools: number;
  /** Consecutive failed watchdog probes. */
  failures: number;
  last_error: string | null;
}

export interface McpServerView {
  id: string;
  name: string;
  transport: McpTransport;
  /** Host identifier used in tool metadata (`mcp_<id>`). */
  host_id: string;
  enabled: boolean;
  health: McpHealth;
}

export interface McpCatalog {
  servers: McpServerView[];
}

export interface McpStateResponse {
  applied: boolean;
  message: string;
  server_id: string;
  enabled: boolean;
}

export interface UpsertMcpServerRequest {
  name?: string | null;
  transport: McpTransport;
}

export interface PluginStateResponse {
  applied: boolean;
  message: string;
  plugin_id: string;
  enabled: boolean;
  host_id: string | null;
}

export interface PluginHost {
  host_id: string;
  runtime?: string;
  pid?: number | null;
  status: string;
  plugins: PluginMeta[];
}

export interface PluginsResponse {
  total: number;
  hosts: PluginHost[];
  plugins: PluginMeta[];
}

export interface InstallPluginResponse {
  plugin_id: string;
  name: string;
  version: string;
  runtime: string;
  commands: CommandDescriptor[];
  tools: ToolDescriptor[];
  status: string;
  message?: string;
}

/**
 * Response of `GET /api/v1/plugins/:id/config`.
 *
 * Field names follow the gateway's wire contract exactly: `values` are the effective settings
 * (schema defaults merged with what is persisted) and `version` is the token used for optimistic
 * concurrency control on the next save.
 */
export interface PluginConfigResponse {
  plugin_id: string;
  values: Record<string, unknown>;
  schema: Record<string, unknown>;
  /** Whether the values came from a persisted file instead of schema defaults alone. */
  persisted: boolean;
  version: number;
}

/** Response of `PUT /api/v1/plugins/:id/config`. */
export interface PluginConfigUpdateResponse {
  plugin_id: string;
  host_id: string;
  values: Record<string, unknown>;
  reloaded: boolean;
  version: number;
}

// Adapter types
/**
 * A platform-dependent feature an adapter implements through the generic adapter contract.
 *
 * Settings that depend on one are shown with the adapters that declare it, so an operator can tell
 * which platforms a switch actually affects.
 */
export type Capability =
  | 'sender_name'
  | 'sender_role'
  | 'group_messages'
  | 'quote_reply'
  | 'forward_content'
  | 'acknowledge'
  | 'member_join'
  | 'bot_join'
  | 'friend_add'
  | 'poke'
  | 'recall'
  | 'friend_requests'
  | 'group_invites';

export interface AdapterItem {
  platform: string;
  kind: 'builtin' | 'plugin';
  display_name: string;
  connected: boolean;
  host_id: string | null;
  plugin_id?: string;
  /** Features this adapter declares. */
  capabilities: Capability[];
}

export interface AdaptersResponse {
  total: number;
  adapters: AdapterItem[];
}

/**
 * Milky adapter types.
 *
 * The configuration is a node property persisted to data/system.json, so every mutation goes
 * through the backend and is applied to the running adapter without a restart.
 */
export type MilkyTransport = 'sse' | 'websocket';

export type MilkyConnectionState =
  | 'disabled'
  | 'connecting'
  | 'connected'
  | 'error';

export interface MilkyConfig {
  enabled: boolean;
  platform: string;
  display_name: string | null;
  base_url: string;
  transport: MilkyTransport;
}

export interface MilkyLogin {
  uin: number;
  nickname: string;
}

export interface MilkyImplementation {
  impl_name: string;
  impl_version: string;
  qq_protocol_version: string;
  qq_protocol_type: string;
  milky_version: string;
}

export interface MilkyStatus {
  platform: string;
  display_name: string;
  enabled: boolean;
  base_url: string;
  transport: MilkyTransport;
  /** Whether a credential is stored; the value itself is never reported. */
  token_configured: boolean;
  state: MilkyConnectionState;
  connected: boolean;
  last_error: string | null;
  events_received: number;
  messages_ingested: number;
  messages_rejected: number;
  messages_delivered: number;
  last_event_at_unix_ms: number | null;
  login: MilkyLogin | null;
  implementation: MilkyImplementation | null;
}

export interface MilkyConfigView {
  config: MilkyConfig;
  status: MilkyStatus;
}

export interface MilkyConfigRequest {
  enabled: boolean;
  platform: string;
  display_name: string | null;
  base_url: string;
  transport: MilkyTransport;

  /** Omitted or empty keeps the stored credential. */
  access_token?: string;
  /** Explicitly removes the stored credential. */
  clear_access_token?: boolean;
}

export interface MilkyTestRequest {
  base_url: string;
  access_token?: string;
}

export interface MilkyTestReport {
  latency_ms: number;
  login: MilkyLogin;
  implementation: MilkyImplementation;
}

// Sessions & Personas
export interface SessionSummary {
  session_id: string;
  session_key?: string;
  turn_count: number;
  total_tokens_used: number;
  active_persona?: string;
  persona_id?: string | null;
  last_updated_at?: number;
  last_active_at?: number;
}

export interface SessionsResponse {
  total: number;
  items?: SessionSummary[];
  sessions?: SessionSummary[];
  page?: number;
  page_size?: number;
  total_pages?: number;
}

/** Where a persona comes from, which decides who may change it. */
export type PersonaKind = 'builtin' | 'custom' | 'instance';

export interface PersonaItem {
  /** Identifier used by the session and instance persona fields. */
  id: string;
  name: string;
  description: string;
  /** The system prompt, exactly as it is sent to the model. */
  prompt: string;
  /**
   * `builtin` ships with the node (read-only), `custom` belongs to the operator, `instance` is
   * generated from a bot instance's own prompt and edited on that instance.
   */
  kind: PersonaKind;
  /** Bot instances that select this persona, which is what blocks its removal. */
  used_by: string[];
}

export interface PersonasResponse {
  total: number;
  /** The persona a conversation uses when none was chosen. */
  base_persona_id: string;
  personas: PersonaItem[];
}

/** Body of `POST /api/v1/personas`; the id is derived from the name when omitted. */
export interface CreatePersonaRequest {
  id?: string;
  name: string;
  description?: string;
  prompt: string;
}

/** Body of `PUT /api/v1/personas/{id}`; the id never changes. */
export interface UpdatePersonaRequest {
  name: string;
  description?: string;
  prompt: string;
}

// WebSocket Logs
export type LogLevel = 'DEBUG' | 'INFO' | 'WARN' | 'ERROR';

export interface LogRecord {
  seq?: number;
  timestamp_ms: number;
  level: LogLevel;
  target: string;
  message: string;
  fields?: Record<string, unknown>;
}

// WebSocket Pipeline & Agent Trace Events
export interface PipelineEventPayload {
  stage: string;
  event_id?: string;
  platform?: string;
  channel_id?: string;
  sender_id?: string;
  command?: string;
  plugin_id?: string;
  host_id?: string;
  session_id?: string;
  content_length?: number;
  segment_count?: number;
  message_id?: string;
  reason?: string;
  phase?: string;
  host_count?: number;
  tool_name?: string;
  error?: string;
  [key: string]: unknown;
}

export interface TraceRecord {
  /**
   * Node-local render key, assigned by the store.
   *
   * Never the server's bus sequence: that counter restarts with the core while the store keeps
   * records across reconnects, and a duplicate keyed-`{#each}` key breaks rendering.
   */
  seq: number;
  /** Server-assigned sequence, retained for diagnostics only. */
  server_seq?: number;
  timestamp_ms: number;
  event: PipelineEventPayload;
}

// Chat Sandbox types
export interface ChatCompletionRequest {
  session_id: string;
  message: string;
  model?: string;
  persona?: string;
  persona_id?: string;
  tools?: boolean;
  protocol?: string;
  base_url?: string;
  api_key?: string;
}

export interface ExecutedTool {
  call_id: string;
  tool_name: string;
  plugin_id: string;
  host_id: string;
  success: boolean;
}

export interface ChatCompletionResponse {
  session_id: string;
  content: string;
  turns: number;
  finish_reason?: string;
  executed_tools: ExecutedTool[];
}

// QQ Official Adapter QR Login & Polling
export interface QQOfficialQrLoginResponse {
  task_id: string;
  bind_key: string;
  qrcode_url: string;
  poll_interval_seconds: number;
}

/** On `created` the node has already applied and saved the credentials; the secret stays there. */
export interface QQOfficialPollLoginResponse {
  status: 'pending' | 'created' | 'expired';
  qr_status?: number;
  appid?: string;
}

/** Gateway session lifecycle of the built-in QQ Official adapter. */
export type QqOfficialConnectionState =
  | 'disabled'
  | 'connecting'
  | 'connected'
  | 'disconnected'
  | 'stopped';

/** Stored QQ Official configuration with the write-only AppSecret removed. */
export interface QqOfficialConfig {
  enabled: boolean;
  app_id: string;
  sandbox: boolean;
  markdown: boolean;
}

/** Live QQ Official gateway status returned by the node. */
export interface QqOfficialStatus {
  platform: string;
  enabled: boolean;
  connected: boolean;
  connection_state: QqOfficialConnectionState;
  secret_configured: boolean;
  bot_name: string | null;
  last_error: string | null;
}

/** Configuration and status shown by the console. */
export interface QqOfficialConfigView {
  config: QqOfficialConfig;
  status: QqOfficialStatus;
}

/** An omitted or empty secret keeps the stored one. */
export interface QqOfficialConfigRequest extends QqOfficialConfig {
  secret?: string;
}

export interface CallPluginToolResponse {
  success: boolean;
  result: unknown;
  error?: string;
}

/** OneBot v11 supports a combined forward socket or a reverse listener. */
export type OneBotTransport = 'forward_websocket' | 'reverse_websocket';

/** The socket's live lifecycle, independent from the saved enabled setting. */
export type OneBotConnectionState =
  | 'disabled'
  | 'connecting'
  | 'listening'
  | 'connected'
  | 'disconnected'
  | 'stopped';

/** Stored configuration with the write-only access token removed. */
export interface OneBotConfig {
  enabled: boolean;
  platform: string;
  display_name: string | null;
  transport: OneBotTransport;
  ws_url: string;
}

/** Live connection status returned by the node. */
export interface OneBotStatus {
  platform: string;
  enabled: boolean;
  connected: boolean;
  connection_state: OneBotConnectionState;
  token_configured: boolean;
  self_id: string | null;
  last_error: string | null;
}

/** Configuration and status shown by the console. */
export interface OneBotConfigView {
  config: OneBotConfig;
  status: OneBotStatus;
}

/** An empty credential preserves the saved token; clearing is explicit. */
export interface OneBotConfigRequest
  extends Omit<
    OneBotConfig,
    'auto_accept_friends' | 'auto_accept_group_invites'
  > {
  /** Omitted keeps the stored choice. */
  auto_accept_friends?: boolean;
  /** Omitted keeps the stored choice. */
  auto_accept_group_invites?: boolean;
  access_token?: string;
  clear_access_token?: boolean;
}
/**
 * Bash switch and execution backend. Who may run it is the explicit administrator list of the
 * command policy; group owners and admins never qualify.
 */
export interface BashPolicy {
  enabled: boolean;
  execution_mode: 'sandbox' | 'local';
  local: BashLocalConfig;
  sandbox: BashSandboxConfig;
}

/** Host execution with an optional pre-execution model review. */
export interface BashLocalConfig {
  working_dir: string;
  auto_review: boolean;
  review_model: string | null;
}

/** Operator-owned runtime; model calls cannot change sandbox limits or mounts. */
export interface BashSandboxConfig {
  endpoint: string;
  image: string;
  network: boolean;
  memory_mb: number;
  cpus: number;
  pids_limit: number;
  file_size_mb: number;
}
