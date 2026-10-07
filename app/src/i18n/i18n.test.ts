import { describe, expect, it } from 'vitest';
import en from './en.json';
import pt from './pt-PT.json';
import { translate } from './index';
import { errorText, skipText } from '../reasons';
import { formatBytes } from '../format';

describe('translate', () => {
  it('fills placeholders and falls back to English, then to the key', () => {
    expect(translate('en', 'pairs.notConnected', { label: 'USB' })).toBe('⚠ USB not connected');
    expect(translate('pt-PT', 'pairs.notConnected', { label: 'USB' })).toBe('⚠ USB não está ligado');
    expect(translate('pt-PT', 'no.such.key')).toBe('no.such.key');
  });

  it('has the same keys in both languages', () => {
    expect(Object.keys(pt).sort()).toEqual(Object.keys(en).sort());
  });

  it('turns engine codes and app errors into text', () => {
    const tx = (k: string, p?: Record<string, string | number>) => translate('pt-PT', k, p);
    expect(skipText(tx, { code: 'folderNotEmpty' })).toBe('a pasta não está vazia');
    expect(errorText(tx, 'pt-PT', { code: 'apply.spaceShortfall', params: { bytes: '1536' } })).toBe(
      'Não há espaço suficiente no disco da cópia: faltam 1,5 KB.',
    );
  });

  it('formats bytes per language', () => {
    expect(formatBytes('en', 1536)).toBe('1.5 KB');
    expect(formatBytes('pt-PT', 1536)).toBe('1,5 KB');
  });
});
