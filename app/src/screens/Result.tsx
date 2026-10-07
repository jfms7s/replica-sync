import { useState } from 'react';
import type { Navigate } from '../App';
import { api, type RunView } from '../api';
import Modal from '../components/Modal';
import { useT } from '../i18n';
import { errorText, failText, stopText } from '../reasons';

export default function Result({ run, pairName, navigate }: { run: RunView; pairName: string; navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const { report } = run;
  const count = (k: 'applied' | 'skipped' | 'failed') => report.results.filter((r) => r.outcome.kind === k).length;
  const failed = report.results.flatMap((r) => (r.outcome.kind === 'failed' ? [{ path: r.path, reason: r.outcome.reason }] : []));
  const [oldRuns, setOldRuns] = useState(run.oldTrashRuns);
  const [confirmEmpty, setConfirmEmpty] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  const emptyOld = async () => {
    setConfirmEmpty(false);
    try {
      for (const id of oldRuns) await api.emptyRun(run.pairId, id);
      setOldRuns([]);
    } catch (e) {
      setMessage(errorText(tx, lang, e));
    }
  };

  return (
    <main className="screen">
      <h1>{t('result.title')} · {pairName}</h1>
      <p>{t('result.summary', { applied: count('applied'), skipped: count('skipped'), failed: count('failed') })}</p>
      {report.stopped && <p className="warn">{t('result.stopped', { reason: stopText(tx, report.stopped) })}</p>}
      {run.warnings.map((w, i) => <p key={i} className="warn">{t('result.warning', { text: errorText(tx, lang, w) })}</p>)}
      {failed.length > 0 && (
        <section className="panel">
          <h2>{t('result.failures')}</h2>
          <ul>{failed.map((f) => <li key={f.path}>{f.path}: {failText(tx, f.reason)}</li>)}</ul>
        </section>
      )}
      {oldRuns.length > 0 && (
        <p className="panel row spread">
          {t('result.oldTrash', { count: oldRuns.length, days: run.trashDays })}
          <button onClick={() => setConfirmEmpty(true)}>{t('result.emptyOld')}</button>
        </p>
      )}
      {message && <p className="error" role="alert">{message}</p>}
      <div className="bar">
        <div className="row">
          {run.logPath && <button onClick={() => void api.openPath(run.logPath!)}>{t('result.openLog')}</button>}
        </div>
        <div className="row">
          {failed.length > 0 && (
            <button onClick={() => navigate({ name: 'applying', mode: 'retry', pairName })}>{t('result.retry')}</button>
          )}
          <button className="primary" onClick={() => navigate({ name: 'pairs' })}>{t('common.done')}</button>
        </div>
      </div>
      {confirmEmpty && (
        <Modal
          title={t('result.emptyOld')}
          actions={<>
            <button onClick={() => setConfirmEmpty(false)}>{t('common.cancel')}</button>
            <button className="primary" onClick={() => void emptyOld()}>{t('result.emptyOld')}</button>
          </>}
        >
          {t('result.emptyOldConfirm', { count: oldRuns.length })}
        </Modal>
      )}
    </main>
  );
}
