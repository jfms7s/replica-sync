import { useEffect, useState } from 'react';
import type { Navigate } from '../App';
import { api, pickFolder, type PairView } from '../api';
import { useT } from '../i18n';
import Modal from '../components/Modal';
import { errorText, isAppError } from '../reasons';

export default function PairEditor({ pair, navigate }: { pair: PairView | null; navigate: Navigate }) {
  const { t, tx, lang } = useT();
  const existing = pair?.pair ?? null;
  const [name, setName] = useState(existing?.name ?? '');
  const [source, setSource] = useState<string | null>(null);
  const [replica, setReplica] = useState<string | null>(null);
  const [rules, setRules] = useState((existing?.user_rules ?? []).join('\n'));
  const [trashDays, setTrashDays] = useState(String(existing?.trash_days ?? 30));
  const [sameVolumeOk, setSameVolumeOk] = useState(false);
  const [askSameVolume, setAskSameVolume] = useState(false);
  const [builtins, setBuiltins] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  useEffect(() => {
    api.builtinRules().then(setBuiltins).catch(() => {});
    if (!existing) api.getSettings().then((s) => setTrashDays(String(s.settings.defaultTrashDays))).catch(() => {});
  }, [existing]);

  // Empty, fractional or below 1 is never sent (0 would offer to empty the run just made).
  const days = Number(trashDays);
  const daysValid = trashDays.trim() !== '' && Number.isInteger(days) && days >= 1;

  const save = async () => {
    if (!daysValid) return;
    setError(null);
    try {
      await api.savePair({
        id: existing?.id ?? null,
        name,
        source,
        replica,
        userRules: rules.split('\n').map((r) => r.trim()).filter(Boolean),
        trashDays: days,
        allowSameVolume: sameVolumeOk,
      });
      navigate({ name: 'pairs' });
    } catch (e) {
      if (isAppError(e) && e.code === 'pair.sameVolume') setAskSameVolume(true);
      setError(errorText(tx, lang, e));
    }
  };

  const remove = async () => {
    if (!existing) return;
    try {
      await api.deletePair(existing.id);
      navigate({ name: 'pairs' });
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
  };

  const folderField = (label: string, value: string | null, set: (v: string) => void, current: string | null) => (
    <div className="panel">
      <span>{label}</span>
      <div className="row">
        <span className="muted">{value ?? current ?? t('editor.notChosen')}</span>
        <button onClick={async () => { const f = await pickFolder(label); if (f) set(f); }}>
          {existing ? t('editor.change') : t('common.browse')}
        </button>
      </div>
    </div>
  );

  return (
    <main className="screen">
      <h1>{existing ? t('editor.editTitle') : t('editor.newTitle')}</h1>
      <label>
        {t('editor.name')}
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      {folderField(t('editor.source'), source, setSource,
        existing ? t('editor.current', { label: existing.source.label, path: existing.source.rel_path || '/' }) : null)}
      {folderField(t('editor.replica'), replica, setReplica,
        existing ? t('editor.current', { label: existing.replica.label, path: existing.replica.rel_path || '/' }) : null)}
      <label>
        {t('editor.rules')}
        <textarea rows={4} value={rules} onChange={(e) => setRules(e.target.value)} />
        <span className="muted">{t('editor.rulesHelp')}</span>
      </label>
      <p className="muted">{t('editor.builtins')}: {builtins.join(', ')}</p>
      <label>
        {t('editor.trashDays')}
        <input type="number" min={1} step={1} value={trashDays} aria-invalid={!daysValid}
          onChange={(e) => setTrashDays(e.target.value)} />
      </label>
      {askSameVolume && (
        <label className="row" style={{ flexDirection: 'row' }}>
          <input type="checkbox" checked={sameVolumeOk} onChange={(e) => setSameVolumeOk(e.target.checked)} />
          {t('editor.sameVolume')}
        </label>
      )}
      {error && <p className="error" role="alert">{error}</p>}
      <div className="bar">
        <div>{existing && <button onClick={() => setConfirmDelete(true)}>{t('editor.delete')}</button>}</div>
        <div className="row">
          <button onClick={() => navigate({ name: 'pairs' })}>{t('common.cancel')}</button>
          <button className="primary" disabled={!daysValid} onClick={() => void save()}>{t('common.save')}</button>
        </div>
      </div>
      {confirmDelete && existing && (
        <Modal
          title={t('editor.delete')}
          actions={<>
            <button onClick={() => setConfirmDelete(false)}>{t('common.cancel')}</button>
            <button className="primary" onClick={() => void remove()}>{t('editor.delete')}</button>
          </>}
        >
          {t('editor.deleteConfirm', { name: existing.name })}
        </Modal>
      )}
    </main>
  );
}
