import { useEffect, useState } from 'react';
import type { Navigate } from '../App';
import { api, onEvent, type AppError, type JobDone, type PreviewSummary, type ScanProgress } from '../api';
import { formatBytes, formatCount } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';

export default function Scanning({ pairId, pairName, navigate }: { pairId: string; pairName: string; navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const [progress, setProgress] = useState<ScanProgress | null>(null);
  const [error, setError] = useState<AppError | null>(null);

  useEffect(() => {
    let alive = true;
    const offs: (() => void)[] = [];
    (async () => {
      offs.push(await onEvent<ScanProgress>('scan-progress', (p) => alive && setProgress(p)));
      offs.push(await onEvent<JobDone<PreviewSummary>>('scan-done', (d) => {
        if (!alive) return;
        if (d.ok) navigate({ name: 'preview', summary: d.ok });
        else if (d.error?.code === 'scan.cancelled') navigate({ name: 'pairs' });
        else setError(d.error);
      }));
      try {
        await api.startScan(pairId); // only after both listeners are in place
      } catch (e) {
        if (alive) setError(e as AppError);
      }
    })();
    return () => {
      alive = false;
      offs.forEach((off) => off());
    };
  }, [pairId, navigate]);

  const side = (label: string, p: ScanProgress['source'] | undefined) => (
    <div className="panel">
      <strong>{label}</strong>
      <p>{t('scan.counts', { files: formatCount(lang, p?.files ?? 0), bytes: formatBytes(lang, p?.bytes ?? 0) })}</p>
      {p?.current && <p className="muted">{t('scan.current', { folder: p.current })}</p>}
    </div>
  );
  const pct = progress?.approxFiles ? Math.min(100, (progress.source.files / progress.approxFiles) * 100) : null;

  return (
    <main className="screen">
      <h1>{t('scan.title', { name: pairName })}</h1>
      {pct !== null && <div className="progress"><div style={{ width: `${pct}%` }} /></div>}
      {side(t('scan.source'), progress?.source)}
      {side(t('scan.replica'), progress?.replica)}
      {error && <p className="error" role="alert">{errorText(tx, lang, error)}</p>}
      <div className="bar">
        <div>{error?.code === 'scan.nested' && (
          <button onClick={() => void api.listPairs().then((ps) => {
            const v = ps.find((p) => p.pair.id === pairId);
            navigate(v ? { name: 'editor', pair: v } : { name: 'pairs' });
          })}>{t('scan.editPair')}</button>
        )}</div>
        {error ? (
          <button onClick={() => navigate({ name: 'pairs' })}>{t('common.back')}</button>
        ) : (
          <button onClick={() => void api.cancelScan()}>{t('common.cancel')}</button>
        )}
      </div>
    </main>
  );
}
