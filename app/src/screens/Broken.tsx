import { useState } from 'react';
import { api } from '../api';
import { useT } from '../i18n';
import { errorText, isAppError } from '../reasons';

/** Startup failed: normally `store.unreadable`, but any rejection is shown safely. */
export default function Broken({ error }: { error: unknown }) {
  const { t, tx, lang } = useT();
  const [openError, setOpenError] = useState<string | null>(null);
  const body = isAppError(error)
    ? t('broken.body', { path: error.params?.path ?? '', detail: error.params?.detail ?? '' })
    : errorText(tx, lang, error);
  return (
    <main className="screen">
      <h1>{t('broken.title')}</h1>
      <p>{body}</p>
      <div className="row">
        <button onClick={() => { api.openDataFolder().catch((e) => setOpenError(errorText(tx, lang, e))); }}>{t('broken.openFolder')}</button>
      </div>
      {openError && <p className="error" role="alert">{openError}</p>}
    </main>
  );
}
