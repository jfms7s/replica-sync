import { useCallback, useEffect, useState } from 'react';
import type { Navigate, Screen } from '../App';
import { api, type TrashRunContents, type TrashRunInfo } from '../api';
import Modal from '../components/Modal';
import { formatBytes, formatRunId } from '../format';
import { useT } from '../i18n';
import { errorText, isAppError } from '../reasons';

/** `back` is where Back goes (the Preview when opened from it); the pairs list by default. */
export default function Trash({ pairId, pairName, back, navigate }: {
  pairId: string; pairName: string; back?: Screen; navigate: Navigate;
}) {
  const { t, tx, lang } = useT();
  const [runs, setRuns] = useState<TrashRunInfo[] | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [contents, setContents] = useState<TrashRunContents | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [conflict, setConflict] = useState<{ run: string; paths: string[]; path: string } | null>(null);
  const [emptying, setEmptying] = useState<TrashRunInfo | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const fail = useCallback((e: unknown) => setMessage(errorText(tx, lang, e)), [tx, lang]);
  const loadRuns = useCallback(() => {
    api.trashRuns(pairId).then(setRuns).catch(fail);
  }, [pairId, fail]);
  useEffect(loadRuns, [loadRuns]);

  const expand = async (run: string) => {
    if (open === run) {
      setOpen(null);
      return;
    }
    setBusy(true);
    try {
      setContents(await api.trashContents(pairId, run));
      setOpen(run);
      setPicked(new Set());
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  const restore = async (run: string, paths: string[], replace: boolean) => {
    setConflict(null);
    setBusy(true);
    try {
      const n = await api.restore(pairId, run, paths, replace);
      setMessage(t('trash.restored', { count: n }));
      setOpen(null);
      loadRuns();
    } catch (e) {
      if (isAppError(e) && e.code === 'trash.conflict') setConflict({ run, paths, path: e.params.path ?? '' });
      else fail(e);
    } finally {
      setBusy(false);
    }
  };

  const restoreAll = async (run: string) => {
    setBusy(true);
    try {
      const c = await api.trashContents(pairId, run);
      setBusy(false);
      await restore(run, c.items.map((i) => i.path), false);
    } catch (e) {
      setBusy(false);
      fail(e);
    }
  };

  const empty = async (run: TrashRunInfo) => {
    setEmptying(null);
    setBusy(true);
    try {
      await api.emptyRun(pairId, run.id);
      if (open === run.id) setOpen(null);
      loadRuns();
    } catch (e) {
      fail(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <main className="screen">
      <h1>{t('trash.title', { name: pairName })}</h1>
      {message && <p role="status">{message}</p>}
      {runs === null ? (
        <p className="muted">{t('common.loading')}</p>
      ) : runs.length === 0 ? (
        <p className="panel">{t('trash.empty')}</p>
      ) : (
        runs.map((r) => (
          <section className="panel" key={r.id}>
            <div className="row spread">
              <button className="link" disabled={busy} onClick={() => void expand(r.id)}>
                {open === r.id ? '▾' : '▸'} {t('trash.run', { date: formatRunId(lang, r.id), files: r.files, bytes: formatBytes(lang, r.bytes) })}
              </button>
              <div className="row">
                <button disabled={busy} onClick={() => void restoreAll(r.id)}>
                  {t('trash.restoreRun')}
                </button>
                <button disabled={busy} onClick={() => setEmptying(r)}>{t('trash.emptyRun')}</button>
              </div>
            </div>
            {open === r.id && contents && (
              <div>
                <ul>
                  {contents.items.map((i) => (
                    <li key={i.path}>
                      <label className="row" style={{ flexDirection: 'row' }}>
                        <input type="checkbox" aria-label={i.path} checked={picked.has(i.path)}
                          onChange={(e) => setPicked((prev) => {
                            const next = new Set(prev);
                            if (e.target.checked) next.add(i.path); else next.delete(i.path);
                            return next;
                          })} />
                        {i.path} <span className="muted">· {t(i.reason === 'deleted' ? 'trash.reason.deleted' : 'trash.reason.replaced')} · {formatBytes(lang, i.size)}</span>
                      </label>
                    </li>
                  ))}
                </ul>
                {contents.unrecorded.length > 0 && (
                  <p className="muted">{t('trash.unrecorded')}: {contents.unrecorded.join(', ')}</p>
                )}
                <button disabled={busy || picked.size === 0} onClick={() => void restore(r.id, [...picked], false)}>
                  {t('trash.restoreSelected')}
                </button>
              </div>
            )}
          </section>
        ))
      )}
      <div className="bar">
        <button onClick={() => navigate(back ?? { name: 'pairs' })}>{t('common.back')}</button>
      </div>
      {conflict && (
        <Modal
          title={t('trash.replaceTitle')}
          actions={<>
            <button onClick={() => setConflict(null)}>{t('common.cancel')}</button>
            <button className="primary" disabled={busy} onClick={() => void restore(conflict.run, conflict.paths, true)}>{t('trash.replace')}</button>
          </>}
        >
          {t('trash.replaceBody', { path: conflict.path })}
        </Modal>
      )}
      {emptying && (
        <Modal
          title={t('trash.emptyRun')}
          actions={<>
            <button onClick={() => setEmptying(null)}>{t('common.cancel')}</button>
            <button className="primary" disabled={busy} onClick={() => void empty(emptying)}>{t('trash.emptyRun')}</button>
          </>}
        >
          {t('trash.emptyConfirm', { files: emptying.files })}
        </Modal>
      )}
    </main>
  );
}
