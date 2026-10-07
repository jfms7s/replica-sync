import { useEffect, useRef, useState } from 'react';
import type { Navigate } from '../App';
import { api, onEvent, type ApplyProgress, type JobDone, type RunView } from '../api';
import { formatBytes, formatCount, formatDuration } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';

export default function Applying({ mode, pairName, navigate }: { mode: 'apply' | 'retry'; pairName: string; navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const [p, setP] = useState<ApplyProgress | null>(null);
  const [paused, setPaused] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [speed, setSpeed] = useState(0); // bytes per second, smoothed
  const [busy, setBusy] = useState(false);
  const [commandError, setCommandError] = useState<string | null>(null);
  const last = useRef<{ at: number; bytes: number } | null>(null);

  useEffect(() => {
    let alive = true;
    const offs: (() => void)[] = [];
    (async () => {
      const offProgress = await onEvent<ApplyProgress>('apply-progress', (next) => {
        if (!alive) return;
        const now = performance.now();
        if (last.current && now > last.current.at) {
          const inst = ((next.bytes_done - last.current.bytes) * 1000) / (now - last.current.at);
          setSpeed((s) => (s === 0 ? inst : s * 0.8 + inst * 0.2));
        }
        last.current = { at: now, bytes: next.bytes_done };
        setP(next);
      });
      offs.push(offProgress);
      if (!alive) return offProgress();
      const offDone = await onEvent<JobDone<RunView>>('apply-done', (d) => {
        if (!alive) return;
        if (d.ok) navigate({ name: 'result', run: d.ok, pairName });
        else setError(errorText(tx, lang, d.error));
      });
      offs.push(offDone);
      if (!alive) return offDone();
      try {
        await (mode === 'retry' ? api.retryFailed() : api.apply()); // only after both listeners are in place
      } catch (e) {
        if (alive) setError(errorText(tx, lang, e));
      }
    })();
    return () => {
      alive = false;
      offs.forEach((off) => off());
    };
    // Start exactly once per mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const command = async (run: () => Promise<unknown>, onOk?: () => void) => {
    setBusy(true);
    setCommandError(null);
    try {
      await run();
      onOk?.();
    } catch (e) {
      setCommandError(errorText(tx, lang, e));
    } finally {
      setBusy(false);
    }
  };

  const pct = p && p.bytes_total > 0 ? (p.bytes_done / p.bytes_total) * 100 : p && p.changes_total > 0 ? (p.changes_done / p.changes_total) * 100 : 0;
  const eta = p && speed > 0 ? (p.bytes_total - p.bytes_done) / speed : NaN;

  return (
    <main className="screen">
      <h1>{t('apply.title')}</h1>
      <div className="progress" aria-label={t('apply.title')}><div style={{ width: `${pct}%` }} /></div>
      {p && (
        <>
          <p>{t('apply.progress', {
            done: formatCount(lang, p.changes_done), total: formatCount(lang, p.changes_total),
            bytes: formatBytes(lang, p.bytes_done), totalBytes: formatBytes(lang, p.bytes_total),
          })}</p>
          {p.current && <p className="muted">{t('apply.current', { path: p.current })}</p>}
          {speed > 0 && <p className="muted">{t('apply.speed', { speed: formatBytes(lang, speed), eta: formatDuration(eta) })}</p>}
        </>
      )}
      {paused && <p className="warn">{t('apply.paused')}</p>}
      {(error ?? commandError) && <p className="error" role="alert">{error ?? commandError}</p>}
      <div className="bar">
        <span />
        {error ? (
          <button onClick={() => navigate({ name: 'pairs' })}>{t('common.back')}</button>
        ) : (
          <div className="row">
            {paused ? (
              <button disabled={busy} onClick={() => void command(() => api.resume(), () => setPaused(false))}>{t('apply.resume')}</button>
            ) : (
              <button disabled={busy} onClick={() => void command(() => api.pause(), () => setPaused(true))}>{t('apply.pause')}</button>
            )}
            <button disabled={busy} onClick={() => void command(() => api.cancel())}>{t('apply.cancel')}</button>
          </div>
        )}
      </div>
    </main>
  );
}
