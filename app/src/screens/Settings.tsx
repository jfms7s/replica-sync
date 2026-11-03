import { useEffect, useRef, useState } from 'react';
import type { Navigate } from '../App';
import { api, type Settings as S, type SettingsView } from '../api';
import { useT, type Lang } from '../i18n';
import { errorText } from '../reasons';
import { MAX_TRASH_DAYS, parseTrashDays } from '../trashDays';

export default function Settings({ navigate, onLanguage }: { navigate: Navigate; onLanguage: (l: Lang) => void }) {
  const { t, tx, lang } = useT();
  const [settings, setSettings] = useState<S | null>(null);
  const [days, setDays] = useState('');
  const [message, setMessage] = useState<string | null>(null);

  const show = (v: SettingsView) => { setSettings(v.settings); setDays(String(v.settings.defaultTrashDays)); };

  // Once: a later language change must not refetch over what the user typed.
  const loaded = useRef(false);
  useEffect(() => {
    if (loaded.current) return;
    loaded.current = true;
    api.getSettings().then(show).catch((e) => setMessage(errorText(tx, lang, e)));
  }, [tx, lang]);

  if (!settings) return <main className="screen"><p className="muted">{t('common.loading')}</p></main>;

  // Empty, fractional, below 1 or above the maximum is never sent.
  const daysNumber = parseTrashDays(days);
  const daysValid = daysNumber !== null;

  const save = async () => {
    if (daysNumber === null) return;
    try {
      const v = await api.setSettings({ ...settings, defaultTrashDays: daysNumber });
      show(v);
      onLanguage(v.resolvedLanguage);
      setMessage(t('settings.saved'));
    } catch (e) {
      setMessage(errorText(tx, lang, e));
    }
  };

  return (
    <main className="screen">
      <h1>{t('settings.title')}</h1>
      <label>
        {t('settings.language')}
        <select value={settings.language} onChange={(e) => setSettings({ ...settings, language: e.target.value as S['language'] })}>
          <option value="auto">{t('settings.lang.auto')}</option>
          <option value="en">{t('settings.lang.en')}</option>
          <option value="pt-PT">{t('settings.lang.pt')}</option>
        </select>
      </label>
      <label>
        {t('settings.trashDays')}
        <input type="number" min={1} max={MAX_TRASH_DAYS} step={1} value={days} aria-invalid={!daysValid}
          onChange={(e) => setDays(e.target.value)} />
      </label>
      <div className="row">
        <button onClick={() => { api.openLogsFolder().catch((e) => setMessage(errorText(tx, lang, e))); }}>{t('settings.openLogs')}</button>
      </div>
      {message && <p role="status">{message}</p>}
      <div className="bar">
        <button onClick={() => navigate({ name: 'pairs' })}>{t('common.back')}</button>
        <button className="primary" disabled={!daysValid} onClick={() => void save()}>{t('common.save')}</button>
      </div>
    </main>
  );
}
