import type { AppError, FailReason, SkipReason, StopReason } from './api';
import { formatBytes } from './format';
import type { Lang, Params } from './i18n';

type Tx = (key: string, params?: Params) => string;

export function skipText(tx: Tx, r: SkipReason): string {
  return tx(`reason.skip.${r.code}`, 'detail' in r ? { detail: r.detail } : {});
}

export function failText(tx: Tx, r: FailReason): string {
  return tx(`reason.fail.${r.code}`, 'detail' in r ? { detail: r.detail } : {});
}

export function stopText(tx: Tx, r: StopReason): string {
  return tx(`reason.stop.${r.code}`);
}

export function isAppError(e: unknown): e is AppError {
  return typeof e === 'object' && e !== null && 'code' in e && typeof (e as AppError).code === 'string';
}

/** Any thrown value as translated text; byte counts are formatted. */
export function errorText(tx: Tx, lang: Lang, e: unknown): string {
  if (!isAppError(e)) return tx('common.error') + (e ? `: ${String(e)}` : '');
  const params: Params = { ...e.params };
  if (e.params.bytes) params.bytes = formatBytes(lang, Number(e.params.bytes));
  return tx(`error.${e.code}`, params);
}
