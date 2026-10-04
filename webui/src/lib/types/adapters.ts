/** Platform and shell configuration contracts. */
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
  | 'group_invites'
  | 'platform_api'
  | 'send_image'
  | 'send_voice'
  | 'send_video'
  | 'send_file';

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
  enabled?: boolean;
  platform?: string;
  display_name?: string | null;
  base_url?: string;
  transport?: MilkyTransport;

  /** Omitted or empty keeps the stored credential. */
  access_token?: string;
  /** Explicitly removes the stored credential. */
  clear_access_token?: boolean;
}

export interface MilkyTestRequest {
  base_url: string;
  access_token?: string;
  /** Probes without the saved credential, matching the pending removal. */
  clear_access_token?: boolean;
}

export interface MilkyTestReport {
  latency_ms: number;
  login: MilkyLogin;
  implementation: MilkyImplementation;
}

// Sessions & Personas
/** One tracked conversation, as `GET /api/v1/sessions` lists it. */
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
export interface QqOfficialConfigRequest extends Partial<QqOfficialConfig> {
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
export interface OneBotConfigRequest extends Partial<OneBotConfig> {
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
