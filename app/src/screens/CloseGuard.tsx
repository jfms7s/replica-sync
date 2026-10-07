import { useEffect, useState } from 'react';
import { api, onEvent } from '../api';
import Modal from '../components/Modal';
import { useT } from '../i18n';
import { errorText } from '../reasons';

/** Shown when the window is closed while a sync is applying (Rust prevents the close). */
export default function CloseGuard() {
  const { t, tx, lang } = useT();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const stop = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.stopAndClose();
    } catch (e) {
      setError(errorText(tx, lang, e));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    let alive = true;
    let off: (() => void) | undefined;
    void onEvent<null>('close-requested', () => alive && setOpen(true)).then((f) => {
      if (alive) off = f;
      else f();
    });
    return () => {
      alive = false;
      off?.();
    };
  }, []);

  if (!open) return null;
  return (
    <Modal
      title={t('close.title')}
      actions={<>
        <button disabled={busy} onClick={() => { setError(null); setOpen(false); }}>{t('close.keep')}</button>
        <button className="primary" disabled={busy} onClick={() => void stop()}>{t('close.stop')}</button>
      </>}
    >
      {t('close.body')}
      {error && <p className="error" role="alert">{error}</p>}
    </Modal>
  );
}
