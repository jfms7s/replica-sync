import { useCallback, useEffect, useState } from 'react';
import type { Navigate } from '../App';
import { api, pickFolder, type PairView, type SideKind } from '../api';
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
  const [sameVolume, setSameVolume] = useState<{ id: string; side: SideKind; folder: string } | null>(null);

  const link = async (id: string, side: SideKind, folder: string, allowSameVolume: boolean) => {
    try {
      await api.relink(id, side, folder, allowSameVolume);
      load();
    } catch (e) {
      if (!allowSameVolume && isAppError(e) && e.code === 'pair.sameVolume') setSameVolume({ id, side, folder });
      else setError(errorText(tx, lang, e));
    }
  };

  const relink = async (v: PairView, side: SideKind) => {
    const title = side === 'Source'
      ? t('pairs.relinkSource', { label: v.pair.source.label })
      : t('pairs.relinkReplica', { label: v.pair.replica.label });
    setError(null);
    let folder: string | null;
    try {
      folder = await pickFolder(title);
    } catch (e) {
      setError(errorText(tx, lang, e));
      return;
    }
    if (!folder) return;
    await link(v.pair.id, side, folder, false);
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
            const ok = v.sourceConnected && v.replicaConnected;
            return (
              <article className="panel" key={pair.id}>
                <div className="row spread">
                  <h2>{pair.name}</h2>
                  <div className="row">
                    <button className="primary" disabled={!ok} onClick={() => navigate({ name: 'scanning', pairId: pair.id, pairName: pair.name })}>
                      {t('pairs.sync')}
                    </button>
                    <button onClick={() => navigate({ name: 'editor', pair: v })}>{t('pairs.edit')}</button>
                    <button disabled={!v.replicaConnected} onClick={() => navigate({ name: 'trash', pairId: pair.id, pairName: pair.name })}>
                      {t('pairs.trash')}
                    </button>
                    {!v.sourceConnected && !v.replicaConnected ? (
                      <>
                        <button onClick={() => void relink(v, 'Source')}>{t('pairs.relinkSourceButton')}</button>
                        <button onClick={() => void relink(v, 'Replica')}>{t('pairs.relinkReplicaButton')}</button>
                      </>
                    ) : !ok && (
                      <button onClick={() => void relink(v, v.sourceConnected ? 'Replica' : 'Source')}>{t('pairs.relink')}</button>
                    )}
                  </div>
                </div>
                <p className="muted">
                  {pair.source.label} › {pair.source.rel_path || '/'} → {pair.replica.label} › {pair.replica.rel_path || '/'}
                </p>
                {!v.sourceConnected && <p className="warn">{t('pairs.notConnected', { label: pair.source.label })}</p>}
                {!v.replicaConnected && <p className="warn">{t('pairs.notConnected', { label: pair.replica.label })}</p>}
                <p className="muted">
                  {pair.last_sync
                    ? t('pairs.lastSync', { date: formatDate(lang, pair.last_sync.at), applied: pair.last_sync.applied, failed: pair.last_sync.failed })
                      + (pair.last_sync.stopped ? t('pairs.incomplete') : '')
                    : t('pairs.neverSynced')}
                </p>
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
            <button className="primary" onClick={() => { const s = sameVolume; setSameVolume(null); void link(s.id, s.side, s.folder, true); }}>
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
