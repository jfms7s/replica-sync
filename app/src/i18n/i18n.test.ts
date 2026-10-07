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

  it('uses the reviewed European Portuguese wording', () => {
    expect(translate('pt-PT', 'apply.pause')).toBe('Pausar');
    expect(translate('pt-PT', 'editor.builtins')).toBe('Sempre ignorados');
    expect(translate('pt-PT', 'error.scan.planConflict', { path: 'a' })).toMatch(/Por favor, comunique este problema\.$/);
    expect(translate('pt-PT', 'reason.skip.caseCollision')).toMatch(/não consegue guardar os dois$/);
    expect(translate('pt-PT', 'editor.rulesHelp')).toMatch(/^\*\.tmp ignora esses ficheiros em qualquer pasta\./);
    expect(translate('pt-PT', 'preview.deleteBanner', { count: 3 })).toBe(
      '⚠ Ficheiros a apagar da cópia: 3. Vão para .sync-trash e podem ser recuperados.');
    expect(translate('pt-PT', 'result.oldTrash', { count: 2, days: 30 })).toBe('Lixos com mais de 30 dias: 2.');
    expect(translate('pt-PT', 'trash.restored', { count: 1 })).toBe('Recuperados: 1');
    expect(translate('pt-PT', 'result.emptyOldConfirm', { count: 2 })).toBe(
      'Apagar definitivamente os lixos antigos (2)? Não é possível desfazer.');
  });

  it('formats bytes per language', () => {
    expect(formatBytes('en', 1536)).toBe('1.5 KB');
    expect(formatBytes('pt-PT', 1536)).toBe('1,5 KB');
  });
});
