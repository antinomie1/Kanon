/** Plugin, skill and MCP catalog contracts. */
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
  author?: string;
  runtime?: string | null;
  /** Project homepage declared by the manifest. */
  homepage?: string;
  /** Source repository declared by the manifest. */
  repository?: string;
  /** Platforms the plugin was written for; empty means every platform. */
  platforms?: string[];
  /** Node versions the plugin supports, as a semver requirement. */
  kanon_version?: string;
  /** Whether the plugin ships console pages under `/api/v1/plugins/<id>/pages/`. */
  has_pages?: boolean;
  /** Whether the running plugin serves HTTP routes under `/api/v1/plugins/<id>/http/`. */
  serves_http?: boolean;
  /** Display-text translations keyed by locale tag (`zh-CN`), then by text key. */
  i18n?: Record<string, Record<string, string>>;
  /** Problems found in the plugin's translation files. */
  i18n_errors?: string[];
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
  /** Cleanup failed after the requested durable change already succeeded. */
  warning?: string;
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

/**
 * JSON body of `POST /api/v1/plugins/install`: exactly one of `path`, `url` or `git`.
 *
 * `replace` must be set to overwrite a plugin already installed under the same id; without it the
 * node answers `409`, so an upgrade is always a deliberate act.
 */
export interface InstallPluginRequest {
  path?: string;
  url?: string;
  git?: string;
  /** Branch or tag to clone; only with `git`. */
  ref?: string;
  replace?: boolean;
}

/** One plugin offered by a market index, annotated for this node. */
export interface MarketPlugin {
  id: string;
  name: string;
  description: string;
  author: string;
  version: string;
  repository?: string;
  download_url?: string;
  kanon_version?: string;
  platforms: string[];
  homepage?: string;
  /** URL of the index that listed it. */
  source: string;
  /** Version installed on this node, when installed. */
  installed_version?: string;
  /** Whether the entry's `kanon_version` admits this node. */
  compatible: boolean;
  incompatible_reason?: string;
}

/** Result of reading one configured market index. */
export interface MarketSource {
  url: string;
  name?: string;
  plugins: number;
  /** Why the index could not be read at all. */
  error?: string;
  /** Entries of this index that were skipped, one sentence each. */
  warnings: string[];
}

/** Response of `GET /api/v1/plugins/market`. */
export interface MarketResponse {
  /** Whether any index is configured in `data/system.json`. */
  configured: boolean;
  hint?: string;
  sources: MarketSource[];
  plugins: MarketPlugin[];
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
