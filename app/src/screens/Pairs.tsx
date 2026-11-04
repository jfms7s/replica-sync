import { useCallback, useEffect, useState } from 'react';
import type { Navigate } from '../App';
import { api, pickFolder, type PairView, type ReplicaView, type SideKind } from '../api';
import { diskName } from '../disks';
import { formatDate } from '../format';
import { useT } from '../i18n';
import Modal from '../components/Modal';
import { errorText, isAppError } from '../reasons';

export default function Pairs({ navigate }: { navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const [pairs, setPairs] = useState<PairView[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    // A later success clears an earlier error (a re-link that failed, then worked).
    api.listPairs().then((ps) => { setPairs(ps); setError(null); }).catch((e) => setError(errorText(tx, lang, e)));
  }, [tx, lang]);
  useEffect(load, [load]);

  // A folder on the other side's drive: asked like a new pair, then retried.
  const [sameVolume, setSameVolume] = useState<{ id: string; side: SideKind; disk: number; folder: string } | null>(null);

  // `disk` is the backup disk to re-link when `side` is 'Replica'.
  const link = async (id: string, side: SideKind, disk: number, folder: string, allowSameVolume: boolean) => {
    try {
      if (side === 'Replica' && disk > 0) await api.relinkReplica(id, disk, folder, allowSameVolume);
      else await api.relink(id, side, folder, allowSameVolume);
      load();
    } catch (e) {
      if (!allowSameVolume && isAppError(e) && e.code === 'pair.sameVolume') setSameVolume({ id, side, disk, folder });
      else setError(errorText(tx, lang, e));
    }
  };

  const relink = async (v: PairView, side: SideKind, disk = 0) => {
    const title = side === 'Source'
      ? t('pairs.relinkSource', { label: v.pair.source.label })
      : t('pairs.relinkReplica', { label: v.replicas[disk].side.label });
    setError(null);
    let folder: string | null;
    try {
      folder = await pickFolder(title);
    } catch (e) {
      setError(errorText(tx, lang, e));
      return;
    }
    if (!folder) return;
    await link(v.pair.id, side, disk, folder, false);
  };

  return (
    <main className="screen">
      <header className="row spread">
        <h1>{t('pairs.title')}</h1>
        <div className="row">
          <button onClick={() => navigate({ name: 'settings' })}>{t('pairs.settings')}</button>
          <button className="primary" onClick={() => navigate({ name: 'editor', pair: null })}>{t('pairs.add')}</button>
        </div>
      </header>
      {error && <p className="error" role="alert">{error}</p>}
      {pairs === null ? (
        <p className="muted">{t('common.loading')}</p>
      ) : pairs.length === 0 ? (
        <p className="panel">{t('pairs.empty')}</p>
      ) : (
        <div className="cards">
          {pairs.map((v) => {
            const { pair } = v;
            const connected = v.replicas.filter((r) => r.connected);
            const rotating = v.replicas.length > 1;
            const ok = v.sourceConnected && connected.length > 0;
            const first = v.replicas[0];
            const lastSyncText = (r: ReplicaView) => (r.lastSync
              ? t('pairs.lastSync', { date: formatDate(lang, r.lastSync.at), applied: r.lastSync.applied, failed: r.lastSync.failed })
                + (r.lastSync.stopped ? t('pairs.incomplete') : '')
              : t('pairs.neverSynced'));
            const toTrash = (r: ReplicaView) => navigate({
              name: 'trash', pairId: pair.id, pairName: pair.name, replica: r.index,
              diskName: rotating ? t('trash.disk', { disk: diskName(t, r.index), label: r.side.label }) : undefined,
            });
            return (
              <article className="panel" key={pair.id}>
                <div className="row spread">
                  <h2>{pair.name}</h2>
                  <div className="row">
                    <button className="primary" disabled={!ok} onClick={() => navigate({ name: 'scanning', pairId: pair.id, pairName: pair.name })}>
                      {rotating ? t('pairs.syncTo', { disk: diskName(t, v.dueReplica) }) : t('pairs.sync')}
                    </button>
                    <button onClick={() => navigate({ name: 'editor', pair: v })}>{t('pairs.edit')}</button>
                    {rotating ? (
                      connected.length === 0
                        ? <button disabled>{t('pairs.trash')}</button>
                        : connected.map((r) => (
                          <button key={r.index} onClick={() => toTrash(r)}>{t('pairs.trashDisk', { disk: diskName(t, r.index) })}</button>
                        ))
                    ) : (
                      <button disabled={!first.connected} onClick={() => toTrash(first)}>{t('pairs.trash')}</button>
                    )}
                    {!v.sourceConnected && !(rotating || first.connected) ? (
                      <>
                        <button onClick={() => void relink(v, 'Source')}>{t('pairs.relinkSourceButton')}</button>
                        <button onClick={() => void relink(v, 'Replica')}>{t('pairs.relinkReplicaButton')}</button>
                      </>
                    ) : !rotating && !ok ? (
                      <button onClick={() => void relink(v, v.sourceConnected ? 'Replica' : 'Source')}>{t('pairs.relink')}</button>
                    ) : rotating && !v.sourceConnected && (
                      <button onClick={() => void relink(v, 'Source')}>{t('pairs.relinkSourceButton')}</button>
                    )}
                  </div>
                </div>
                {rotating ? (
                  <>
                    <p className="muted">{pair.source.label} › {pair.source.rel_path || '/'}</p>
                    <ul className="disks">
                      {v.replicas.map((r) => (
                        <li key={r.index}>
                          <span className="row">
                            <span>
                              {t('pairs.diskLine', { disk: diskName(t, r.index), label: r.side.label, path: r.side.rel_path || '/' })}
                              {' · '}
                              <span className={r.connected ? undefined : 'muted'}>{t(r.connected ? 'pairs.diskConnected' : 'pairs.diskAway')}</span>
                            </span>
                            {!r.connected && (
                              <button className="link" onClick={() => void relink(v, 'Replica', r.index)}>
                                {t('pairs.relinkDisk', { disk: diskName(t, r.index) })}
                              </button>
                            )}
                          </span>
                          <span className="muted">{lastSyncText(r)}</span>
                        </li>
                      ))}
                    </ul>
                    {!v.sourceConnected && <p className="warn">{t('pairs.notConnected', { label: pair.source.label })}</p>}
                    {connected.length === 0 && <p className="warn">{t('pairs.noDiskConnected')}</p>}
                  </>
                ) : (
                  <>
                    <p className="muted">
                      {pair.source.label} › {pair.source.rel_path || '/'} → {first.side.label} › {first.side.rel_path || '/'}
                    </p>
                    {!v.sourceConnected && <p className="warn">{t('pairs.notConnected', { label: pair.source.label })}</p>}
                    {!first.connected && <p className="warn">{t('pairs.notConnected', { label: first.side.label })}</p>}
                    <p className="muted">{lastSyncText(first)}</p>
                  </>
                )}
              </article>
            );
          })}
        </div>
      )}
      {sameVolume && (
        <Modal
          onClose={() => setSameVolume(null)}
          title={t('pairs.sameVolumeTitle')}
          actions={<>
            <button onClick={() => setSameVolume(null)}>{t('common.cancel')}</button>
            <button className="primary" onClick={() => { const s = sameVolume; setSameVolume(null); void link(s.id, s.side, s.disk, s.folder, true); }}>
              {t('editor.sameVolume')}
            </button>
          </>}
        >
          {t('pairs.sameVolumeBody')}
        </Modal>
      )}
    </main>
  );
}
