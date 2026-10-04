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
  header: { id: string };
  cursor: number;
  records: DshRecord[];
  hasMore: boolean;
  projections: unknown;
}

/** Native journal entries are displayed at an immutable remote cursor. */
export interface DshRecord {
  type: string;
  event: {
    seq: number;
    type: string;
    data: Record<string, unknown>;
    surfaceOp?: 'append' | { op: 'replace'; startSeq: number; endSeq: number };
  };
}

/** Earlier native history window, read from the same snapshot cut. */
export interface DshPage {
  records: DshRecord[];
  hasMore: boolean;
}

/** DSH's own model catalog; no builtin provider or capability conversion is applied. */
export interface DshModels {
  default: { provider: string; model: string };
  groups: Array<{ id: string; name: string; models: Array<{ id: string; name: string }> }>;
  failures: Array<{ id: string; name: string; message: string }>;
}
