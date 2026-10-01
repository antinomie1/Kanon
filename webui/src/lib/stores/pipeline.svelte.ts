import { WsRingBuffer, type WsStatus } from '../api/ws';
import type { TraceRecord } from '../types';

class PipelineStore {
  records = $state<TraceRecord[]>([]);

  /**
   * Client-side render key.
   *
   * The server's `seq` comes from a bus counter that restarts with the process, while this store
   * deliberately keeps records across socket reconnects. Reusing the server value as the keyed
   * `{#each}` key therefore produced duplicate keys after a core restart, which throws during
   * render and leaves the panel blank until a full page reload. A counter owned by the store is
   * monotonic for the lifetime of the page and cannot collide.
   */
  private nextSeq = 0;
  /** Records received since the page loaded, including ones the ring buffer has since dropped. */
  received = $state(0);
  status = $state<WsStatus>('disconnected');
  selectedStage = $state<string>('ALL');
  searchQuery = $state<string>('');

  private wsBuffer: WsRingBuffer<unknown>;

  constructor() {
    this.wsBuffer = new WsRingBuffer<unknown>({
      url: '/ws/v1/events',
      capacity: 1000,
      onMessage: (raw: unknown) => {
        let rec: TraceRecord | null = null;
        if (raw && typeof raw === 'object') {
          const frame = raw as Record<string, unknown>;
          if (frame.type === 'trace' && frame.record) {
            rec = frame.record as TraceRecord;
          } else if (frame.event) {
            rec = frame as unknown as TraceRecord;
          }
        }
        if (rec?.event) {
          const rawEvent = rec.event as Record<string, unknown>;
          const stageName = String(
            rawEvent.stage || rawEvent.kind || 'pipeline',
          );
          const normalized: TraceRecord = {
            ...rec,
            server_seq: rec.seq,
            seq: ++this.nextSeq,
            timestamp_ms: rec.timestamp_ms || Date.now(),
            event: {
              ...rec.event,
              stage: stageName,
            },
          };
          this.records = [...this.records.slice(-999), normalized];
          this.received++;
        }
      },
      onStatusChange: (status) => {
        this.status = status;
      },
    });

    if (typeof window !== 'undefined') {
      this.wsBuffer.connect();
    }
  }

  reconnect() {
    this.wsBuffer.reconnect();
  }

  get stats() {
    const counts: Record<string, number> = {
      ingested: 0,
      pre_filter: 0,
      command: 0,
      llm: 0,
      tool: 0,
      outbound: 0,
      breaker: 0,
    };

    for (const rec of this.records) {
      if (!rec?.event) continue;
      const stage = rec.event.stage || '';
      if (stage === 'ingested') counts.ingested++;
      else if (stage.startsWith('pre_filter')) counts.pre_filter++;
      else if (stage.startsWith('command')) counts.command++;
      else if (stage.startsWith('llm')) counts.llm++;
      else if (stage.startsWith('tool')) counts.tool++;
      else if (stage.startsWith('outbound')) counts.outbound++;
      else if (stage === 'circuit_breaker_tripped') counts.breaker++;
    }

    return counts;
  }

  /** Whether a record passes the current stage filter and search. */
  matches(rec: TraceRecord): boolean {
    if (!rec?.event) return false;
    const stage = rec.event.stage || '';
    switch (this.selectedStage) {
      case 'ALL':
        break;
      case 'breaker':
        if (stage !== 'circuit_breaker_tripped') return false;
        break;
      case 'ingested':
        if (stage !== 'ingested') return false;
        break;
      default:
        // The remaining groups (pre_filter, command, llm, tool, outbound) are stage prefixes.
        if (!stage.startsWith(this.selectedStage)) return false;
    }
    const q = this.searchQuery.trim().toLowerCase();
    return !q || JSON.stringify(rec.event).toLowerCase().includes(q);
  }

  get filteredRecords(): TraceRecord[] {
    return this.records.filter((rec) => this.matches(rec));
  }

  clear() {
    this.wsBuffer.clear();
    this.records = [];
  }

  destroy() {
    this.wsBuffer.destroy();
  }
}

export const pipelineStore = new PipelineStore();
