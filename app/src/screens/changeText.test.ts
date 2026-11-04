import { describe, expect, it } from 'vitest';
import type { Change } from '../api';
import { translate, type Lang } from '../i18n';
import { changeDetail } from './changeText';

const MS = 1e6;
// Noon UTC, so the date is the same in every time zone.
const at = (iso: string) => Date.parse(`${iso}T12:00:00Z`) * MS;

function detail(lang: Lang, c: Change): string {
  return changeDetail(
    (k, p) => translate(lang, k, p),
    (k, p) => translate(lang, k, p),
    lang,
    c,
  );
}

const update = (replica_newer: boolean): Change => ({
  type: 'update',
  path: 'a.bin',
  size: 93_184,
  mtime_ns: at('2026-10-04'),
  replica_size: 90_112,
  replica_mtime_ns: at('2026-03-12'),
  replica_newer,
});

describe('changeDetail for an update', () => {
  it('shows the backup side first and the source side second, with dates and sizes', () => {
    expect(detail('en', update(false))).toBe('source is newer: Mar 12, 2026 → Oct 4, 2026, 88.0 KB → 91.0 KB');
  });

  it('keeps the warning and both sides when the backup is the newer one', () => {
    expect(detail('en', update(true))).toBe('⚠ backup is newer: Mar 12, 2026 → Oct 4, 2026, 88.0 KB → 91.0 KB');
  });

  it('is translated and formatted for pt-PT', () => {
    const text = detail('pt-PT', update(false));
    expect(text).toMatch(/^a origem é mais recente: .*2026.* → .*2026.*, 88,0 KB → 91,0 KB$/);
  });
});
