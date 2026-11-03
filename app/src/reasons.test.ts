import { describe, expect, it } from 'vitest';
import { translate } from './i18n';
import { errorText, failText, isAppError, stopText } from './reasons';

const tx = (k: string, p?: Record<string, string | number>) => translate('en', k, p);

describe('reason texts', () => {
  it('translates a fail reason, with its detail', () => {
    expect(failText(tx, { code: 'inUse' })).toBe(translate('en', 'reason.fail.inUse'));
    expect(failText(tx, { code: 'io', detail: 'EIO' })).toContain('EIO');
  });

  it('translates a stop reason', () => {
    expect(stopText(tx, { code: 'replicaFull' })).toBe('the backup drive is full');
  });

  it('shows a non-AppError rejection as the generic error, with its text when there is one', () => {
    expect(errorText(tx, 'en', 'boom')).toBe('Something went wrong: boom');
    expect(errorText(tx, 'en', null)).toBe('Something went wrong');
    expect(isAppError({ code: 1 })).toBe(false);
  });
});
