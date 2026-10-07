import { useEffect, useState } from 'react';
import { api, onEvent } from '../api';
import Modal from '../components/Modal';
import { useT } from '../i18n';

/** Shown when the window is closed while a sync is applying (Rust prevents the close). */
export default function CloseGuard() {
  const { t } = useT();
  const [open, setOpen] = useState(false);

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
        <button onClick={() => setOpen(false)}>{t('close.keep')}</button>
        <button className="primary" onClick={() => void api.stopAndClose()}>{t('close.stop')}</button>
      </>}
    >
      {t('close.body')}
    </Modal>
  );
}
