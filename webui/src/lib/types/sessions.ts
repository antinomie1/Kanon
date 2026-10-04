/** Sessions, chat and activity contracts. */
export interface SessionSummary {
  /**
   * Session key. Instance conversations read `instance:<id>:<conversation>#<generation>`, where
   * the generation grows each time `/new` starts the conversation over.
   */
  session_key: string;
  /** Epoch seconds of the first message. */
  created_at: number;
  /** Epoch seconds of the latest message. */
  last_active_at: number;
  turn_count: number;
  total_tokens_used: number;
  /** Persona bound to the session; `null` uses the base assistant. */
  persona_id: string | null;
}

/** One page of `GET /api/v1/sessions`, most recently active first. */
export interface SessionsResponse {
  items: SessionSummary[];
  page: number;
  page_size: number;
  /** Sessions matching the search, across every page. */
  total: number;
  total_pages: number;
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
  agent?: string;
  instance_id?: string;
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
  agent?: string;
  session_id: string;
  content: string;
  /** Builtin iteration count; DSH exposes its own journal and turn metadata instead. */
  turns?: number;
  finish_reason?: string;
  executed_tools: ExecutedTool[];
}
