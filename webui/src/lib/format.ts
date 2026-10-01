import { i18n } from './stores/i18n.svelte';

/**
 * Human duration for "running for …" labels, at most two units and never seconds past the first
 * minute: an operator wants to know "about how long", not an exact count.
 */
export function formatDuration(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds));
  const days = Math.floor(s / 86400);
  const hours = Math.floor((s % 86400) / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const zh = i18n.locale === 'zh';
  if (days > 0) return zh ? `${days} 天 ${hours} 小时` : `${days}d ${hours}h`;
  if (hours > 0)
    return zh ? `${hours} 小时 ${minutes} 分` : `${hours}h ${minutes}m`;
  if (minutes > 0) return zh ? `${minutes} 分钟` : `${minutes} min`;
  return zh ? `${s} 秒` : `${s}s`;
}

/** Byte count with a binary unit, e.g. `18.4 MB`. */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
}

/** Wall-clock time of a millisecond timestamp, `HH:MM:SS`. */
export function formatClock(ms: number): string {
  return new Date(ms).toLocaleTimeString(
    i18n.locale === 'zh' ? 'zh-CN' : 'en-GB',
    { hour12: false },
  );
}

/** Message text of an unknown thrown value. */
export function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
