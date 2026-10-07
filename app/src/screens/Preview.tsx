import { useEffect, useRef, useState } from 'react';
import type { Navigate } from '../App';
import { api, pickSaveFile, type PreviewSummary } from '../api';
import Modal from '../components/Modal';
import { formatBytes, formatCount } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';
import Tree from './Tree';

/** "Show them" opens at most this many folders, so a huge delete can't freeze the window. */
const SHOW_FOLDERS_LIMIT = 50;

export default function Preview({ summary: initial, navigate }: { summary: PreviewSummary; navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const [s, setS] = useState(initial);
  const [expandRequest, setExpandRequest] = useState<string[] | null>(null);
  const [refreshToken, setRefreshToken] = useState(0);
  const [message, setMessage] = useState<string | null>(null);
  const changed = useRef(false);
  const update = (next: PreviewSummary) => {
    changed.current = true;
    setS(next);
  };

  // Coming back (e.g. from the trash) the given summary may be stale: free
  // space can have changed. Use Rust's current one; keep the given one on error.
  useEffect(() => {
    let alive = true;
    api.previewSummary()
      .then((fresh) => { if (alive && !changed.current) setS(fresh); })
      .catch(() => {});
    return () => { alive = false; };
  }, []);

  const guardOpen = s.guard !== null && !s.guardConfirmed;
  const canApply = !guardOpen && s.shortfall === null && s.selectedCount > 0;
  const run = (f: () => Promise<PreviewSummary>) => f().then((next) => { update(next); setRefreshToken((n) => n + 1); }).catch((e) => setMessage(errorText(tx, lang, e)));

  const savePreview = async () => {
    const file = await pickSaveFile(`${s.pairName}-preview.txt`);
    if (!file) return;
    try {
      await api.savePreview(file);
      setMessage(t('preview.saved'));
    } catch (e) {
      setMessage(errorText(tx, lang, e));
    }
  };

  return (
    <main className="screen">
      <header className="row spread">
        <h1>{t('preview.title', { name: s.pairName })}</h1>
        <span className="muted">
          {t('preview.scanned', {
            files: formatCount(lang, s.source.files + s.replica.files),
            seconds: (Math.max(s.source.elapsed_ms, s.replica.elapsed_ms) / 1000).toFixed(1),
          })}
        </span>
      </header>
      {s.isEmpty ? (
        <p className="panel">{t('preview.inSync')}</p>
      ) : (
        <>
          {s.totals.deletes > 0 && (
            <p className="warn">
              {t('preview.deleteBanner', { count: formatCount(lang, s.totals.deletes) })}{' '}
              <button className="link" onClick={() => setExpandRequest(s.deleteFolders.slice(0, SHOW_FOLDERS_LIMIT))}>{t('preview.showThem')}</button>
            </p>
          )}
          {s.shortfall !== null && (
            <p className="warn">
              {t('preview.shortfall', { bytes: formatBytes(lang, s.shortfall) })}{' '}
              <button className="link"
                onClick={() => navigate({ name: 'trash', pairId: s.pairId, pairName: s.pairName, back: { name: 'preview', summary: s } })}>
                {t('preview.openTrash')}
              </button>
            </p>
          )}
          <Tree onSummary={update} expandRequest={expandRequest} refreshToken={refreshToken} />
        </>
      )}
      {message && <p role="status">{message}</p>}
      <div className="bar">
        <span>
          {t('preview.selected', {
            selected: formatCount(lang, s.selectedCount),
            total: formatCount(lang, s.actionableCount),
            bytes: formatBytes(lang, s.selected.bytes_to_copy),
            skipped: formatCount(lang, s.totals.skipped),
          })}
        </span>
        <div className="row">
          <button onClick={() => navigate({ name: 'pairs' })}>{t('common.back')}</button>
          {!s.isEmpty && <button onClick={() => void savePreview()}>{t('preview.savePreview')}</button>}
          {!s.isEmpty && <button onClick={() => void run(() => api.selectAll(true))}>{t('preview.approveAll')}</button>}
          {!s.isEmpty && (
            <button className="primary" disabled={!canApply}
              onClick={() => navigate({ name: 'applying', mode: 'apply', pairName: s.pairName })}>
              {t('preview.apply')}
            </button>
          )}
        </div>
      </div>
      {guardOpen && s.guard && (
        <Modal
          title={t('preview.guardTitle')}
          actions={<>
            <button onClick={() => navigate({ name: 'pairs' })}>{t('common.back')}</button>
            <button className="primary" onClick={() => void run(api.confirmWrongFolder)}>{t('preview.guardConfirm')}</button>
          </>}
        >
          {t('preview.guardBody', { affected: formatCount(lang, s.guard.affected), total: formatCount(lang, s.guard.replica_files) })}
        </Modal>
      )}
    </main>
  );
}
