/** Transport configuration for the optional DSH agent backend. */
export interface DshConnection {
  base_url: string;
  cookie_file: string | null;
  request_timeout_seconds: number;
  turn_timeout_seconds: number;
}

/** Native, redacted DSH settings and optimistic revision. */
export interface DshSettings {
  writable: boolean;
  hasDocument: boolean;
  namespaces: Array<{
    ns: string;
    revision: number;
    value: unknown;
    schema: unknown;
    secrets: Array<{ path: string[]; set: boolean }>;
  }>;
}

/** DSH's authoritative session listing, without a builtin memory mirror. */
export interface DshSession {
  sessionId: string;
  updatedAt: number;
  running: boolean;
  blank: boolean;
  projections?: { values: { title?: { title: string }; modelSelection?: { next?: { provider: string; model: string } } } };
}

/** A native journal window; older windows must use the same cursor. */
export interface DshSnapshot {
  cursor: number;
  records: Array<{ type: string; event: { seq: number; type: string; data: unknown } }>;
  hasMore: boolean;
  projections: unknown;
}
