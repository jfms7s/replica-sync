import { createContext, useContext, useMemo, type ReactNode } from 'react';
import en from './en.json';
import pt from './pt-PT.json';

export type Lang = 'en' | 'pt-PT';
export type Key = keyof typeof en;
export type Params = Record<string, string | number>;

const tables: Record<Lang, Record<string, string>> = { en, 'pt-PT': pt };

export function translate(lang: Lang, key: string, params: Params = {}): string {
  const text = tables[lang][key] ?? tables.en[key];
  if (text === undefined) {
    if (import.meta.env.DEV) console.warn(`missing translation: ${key}`);
    return key;
  }
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => (name in params ? String(params[name]) : whole));
}

const LangContext = createContext<Lang>('en');

export function I18nProvider({ lang, children }: { lang: Lang; children: ReactNode }) {
  return <LangContext.Provider value={lang}>{children}</LangContext.Provider>;
}

/** `t` for static keys (type-checked), `tx` for keys built at run time (reason codes). */
export function useT() {
  const lang = useContext(LangContext);
  // Memoised so `t`/`tx` are stable between renders and safe in effect deps.
  return useMemo(
    () => ({
      lang,
      t: (key: Key, params?: Params) => translate(lang, key, params),
      tx: (key: string, params?: Params) => translate(lang, key, params),
    }),
    [lang],
  );
}
