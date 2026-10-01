import { WsRingBuffer, type WsStatus } from '../api/ws';
import type { LogLevel, LogRecord } from '../types';

class LogStore {
  records = $state<LogRecord[]>([]);
  status = $state<WsStatus>('disconnected');
  /** Records received since the page loaded, including ones the ring buffer has since dropped. */
  received = $state(0);
  filterLevel = $state<LogLevel | 'ALL'>('ALL');
  searchQuery = $state<string>('');

  private wsBuffer: WsRingBuffer<unknown>;

  constructor() {
    this.wsBuffer = new WsRingBuffer<unknown>({
      url: '/ws/v1/logs',
      capacity: 1000,
      onMessage: (raw: unknown) => {
        let rec: LogRecord | null = null;
        if (raw && typeof raw === 'object') {
          const frame = raw as Record<string, unknown>;
          if (frame.type === 'log' && frame.record) {
            rec = frame.record as LogRecord;
          } else if (frame.level && frame.message) {
            rec = frame as unknown as LogRecord;
          }
        }
        if (rec) {
          const upperLevel = String(rec.level || 'INFO').toUpperCase();
          const normalized: LogRecord = {
            ...rec,
            level: (['INFO', 'WARN', 'ERROR', 'DEBUG'].includes(upperLevel)
              ? upperLevel
              : 'INFO') as LogLevel,
            target: rec.target || 'kanon_core',
            message: rec.message || '',
            timestamp_ms: rec.timestamp_ms || Date.now(),
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

  /** Whether a record passes the current level filter and search. */
  matches(rec: LogRecord): boolean {
    if (!rec?.message) return false;
    const lvl = rec.level ? String(rec.level).toUpperCase() : 'INFO';
    if (this.filterLevel !== 'ALL' && lvl !== this.filterLevel) return false;
    const q = this.searchQuery.trim().toLowerCase();
    return (
      !q ||
      rec.message.toLowerCase().includes(q) ||
      (rec.target?.toLowerCase().includes(q) ?? false) ||
      lvl.toLowerCase().includes(q)
    );
  }

  get filteredRecords(): LogRecord[] {
    return this.records.filter((rec) => this.matches(rec));
  }

  clear() {
    this.wsBuffer.clear();
    this.records = [];
  }

  setFilter(level: LogLevel | 'ALL') {
    this.filterLevel = level;
    if (this.status === 'connected') {
      this.wsBuffer.send({
        action: 'set_filter',
        level: level === 'ALL' ? 'debug' : level.toLowerCase(),
      });
    }
  }

  destroy() {
    this.wsBuffer.destroy();
  }
}

export const logStore = new LogStore();
