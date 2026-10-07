import type { Lang } from './i18n';

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB'];

/** 1024-based like Windows Explorer; decimal separator follows the language. */
export function formatBytes(lang: Lang, n: number): string {
  let v = n;
  let i = 0;
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i += 1;
  }
  const digits = i === 0 ? 0 : 1;
  return `${new Intl.NumberFormat(lang, { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(v)} ${UNITS[i]}`;
}

export function formatCount(lang: Lang, n: number): string {
  return new Intl.NumberFormat(lang).format(n);
}

export function formatDate(lang: Lang, iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(lang, { dateStyle: 'medium', timeStyle: 'short' }).format(d);
}

/** Trash run ids look like 2026-10-07_143200[-2]. */
export function formatRunId(lang: Lang, id: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})_(\d{2})(\d{2})(\d{2})/.exec(id);
  if (!m) return id;
  const [, y, mo, d, h, mi, s] = m;
  return formatDate(lang, `${y}-${mo}-${d}T${h}:${mi}:${s}`);
}

export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—';
  const s = Math.round(seconds);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h > 0 ? `${h} h ${m} min` : m > 0 ? `${m} min ${s % 60} s` : `${s} s`;
}
