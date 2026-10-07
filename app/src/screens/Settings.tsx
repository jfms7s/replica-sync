import { useEffect, useState } from 'react';
import type { Navigate } from '../App';
import { api, type Settings as S } from '../api';
import { useT, type Lang } from '../i18n';
import { errorText } from '../reasons';

export default function Settings({ navigate, onLanguage }: { navigate: Navigate; onLanguage: (l: Lang) => void }) {
  const { t, tx, lang } = useT();
  const [settings, setSettings] = useState<S | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  useEffect(() => {
    api.getSettings().then((v) => setSettings(v.settings)).catch((e) => setMessage(errorText(tx, lang, e)));
  }, [tx, lang]);

  if (!settings) return <main className="screen"><p className="muted">{t('common.loading')}</p></main>;

  const save = async () => {
    try {
      const v = await api.setSettings(settings);
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
        <input type="number" min={1} value={settings.defaultTrashDays}
          onChange={(e) => setSettings({ ...settings, defaultTrashDays: Number(e.target.value) })} />
      </label>
      <div className="row">
        <button onClick={() => void api.openLogsFolder()}>{t('settings.openLogs')}</button>
      </div>
      {message && <p role="status">{message}</p>}
      <div className="bar">
        <button onClick={() => navigate({ name: 'pairs' })}>{t('common.back')}</button>
        <button className="primary" onClick={() => void save()}>{t('common.save')}</button>
      </div>
    </main>
  );
}
