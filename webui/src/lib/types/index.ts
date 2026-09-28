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
  configured: boolean;
  /** Where the effective provider comes from: console selection, environment bootstrap, or none. */
  source: 'console' | 'env' | 'runtime' | 'none';
  protocol: string;
  model: string;
  base_url: string | null;
  api_key_configured: boolean;
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
  memory_window: number;
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

export interface ActiveProviderInfo {
  configured: boolean;
  /** Where the effective provider comes from: console selection, environment bootstrap, or none. */
  source: 'console' | 'env' | 'runtime' | 'none';
  protocol: string;
  /** Canonical `<provider>/<model-id>` reference in effect. */
  model: string;
  /** Model id actually sent upstream (the provider prefix stripped). */
  upstream_model: string;
  /** Provider endpoint serving the default model. */
  provider: string | null;
  base_url: string | null;
  api_key_configured: boolean;
  temperature: number | null;
  max_tokens: number | null;
  context_length: number | null;
  capabilities: ModelCapabilities;
}

/** One configured named endpoint, credential excluded. */
export interface ProviderInfo {
  name: string;
  protocol: string;
  base_url: string;
  api_key_configured: boolean;
  temperature: number | null;
  max_tokens: number | null;
  is_default: boolean;
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
  active: ActiveProviderInfo;
  /** Every configured provider endpoint. */
  providers: ProviderInfo[];
  /** Name of the endpoint used for unprefixed model references. */
  default_provider: string | null;
  /** Canonical model reference the node answers with by default. */
  default_model: string | null;
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
  make_default?: boolean;
  /** Model reference to answer with; required when making a new default. */
  model?: string;
}

/** Request body of `PUT /api/v1/providers/default`. */
export interface SetDefaultProviderRequest {
  provider: string;
  model?: string;
}

/** Request body of `POST /api/v1/providers/delete`. */
export interface DeleteProviderRequest {
  name: string;
}

/** Payload for `PUT /api/v1/providers/active`; mirrors the `KANON_LLM_*` variables. */
export interface ActivateProviderRequest {
  protocol: string;
  base_url: string;
  model: string;
  api_key?: string;
  temperature?: number;
  max_tokens?: number;
  /** Optional endpoint name; the server derives one from the base URL when omitted. */
  provider_name?: string;
}

export interface ActivateProviderResponse {
  applied: boolean;
  message: string;
  active: ActiveProviderInfo;
}

export interface TestProviderRequest {
  protocol?: string;
  base_url?: string;
  api_key?: string;
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
export interface AdapterItem {
  platform: string;
  kind: 'builtin' | 'plugin';
  display_name: string;
  connected: boolean;
  host_id: string | null;
  plugin_id?: string;
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

export interface PersonaItem {
  /** Identifier used by the session and instance persona fields. */
  id: string;
  name: string;
  description: string;
  /** Raw prompt template, including `{{variable}}` slots. */
  template: string;
  variables: string[];
  required_variables: string[];
  default_temperature: number | null;
  default_model: string | null;
}

export interface PersonasResponse {
  total: number;
  personas: PersonaItem[];
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

export interface QQOfficialPollLoginResponse {
  status: 'pending' | 'created' | 'expired' | 'error';
  qr_status: number;
  appid?: string;
  secret?: string;
  saved?: boolean;
  message?: string;
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
export interface OneBotConfigRequest extends OneBotConfig {
  access_token?: string;
  clear_access_token?: boolean;
}
