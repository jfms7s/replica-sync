import { useCallback, useEffect, useState } from 'react';
import type { Navigate } from '../App';
import { api, pickFolder, type PairView, type SideKind } from '../api';
import { formatDate } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';

export default function Pairs({ navigate }: { navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const [pairs, setPairs] = useState<PairView[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api.listPairs().then(setPairs).catch((e) => setError(errorText(tx, lang, e)));
  }, [tx, lang]);
  useEffect(load, [load]);

  const relink = async (v: PairView, side: SideKind) => {
    const title = side === 'Source'
      ? t('pairs.relinkSource', { label: v.pair.source.label })
      : t('pairs.relinkReplica', { label: v.pair.replica.label });
    const folder = await pickFolder(title);
    if (!folder) return;
    try {
      await api.relink(v.pair.id, side, folder);
      load();
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
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
    </main>
  );
}
