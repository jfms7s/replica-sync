import { useEffect, useState, type ReactNode } from 'react';
import type { Navigate } from '../App';
import { api, pickFolder, type Pair, type PairView } from '../api';
import { diskName } from '../disks';
import { useT } from '../i18n';
import Modal from '../components/Modal';
import { errorText, isAppError } from '../reasons';
import { MAX_TRASH_DAYS, parseTrashDays } from '../trashDays';

export default function PairEditor({ pair, navigate }: { pair: PairView | null; navigate: Navigate }) {
  const { t, tx, lang } = useT();
  // The saved pair: given, or the one this editor has saved (the save below is
  // several steps, so a later one that fails leaves the earlier ones done).
  const [existing, setExisting] = useState<Pair | null>(pair?.pair ?? null);
  const [name, setName] = useState(existing?.name ?? '');
  const [source, setSource] = useState<string | null>(null);
  const [replica, setReplica] = useState<string | null>(null);
  // The second backup disk: its folder when one was chosen, and whether its field is open.
  const [second, setSecond] = useState<string | null>(null);
  const [addingSecond, setAddingSecond] = useState(false);
  const [removing, setRemoving] = useState<number | null>(null);
  const [rules, setRules] = useState((existing?.user_rules ?? []).join('\n'));
  const [trashDays, setTrashDays] = useState(String(existing?.trash_days ?? 30));
  const [sameVolumeOk, setSameVolumeOk] = useState(false);
  const [askSameVolume, setAskSameVolume] = useState(false);
  const [builtins, setBuiltins] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [noDefaults, setNoDefaults] = useState(false);

  useEffect(() => {
    api.builtinRules().then(setBuiltins).catch((e) => console.error('builtin rules', e));
    if (!existing) {
      api.getSettings().then((s) => setTrashDays(String(s.settings.defaultTrashDays))).catch((e) => {
        console.error('settings', e);
        setNoDefaults(true);
      });
    }
  }, [existing]);

  // Empty, fractional, below 1 (0 would offer to empty the run just made) or
  // above the maximum is never sent.
  const days = parseTrashDays(trashDays);
  const daysValid = days !== null;

  // A same-drive answer is for the folders it was given for.
  const pickedFolder = (set: (v: string) => void) => (f: string) => {
    set(f);
    setSameVolumeOk(false);
    setAskSameVolume(false);
  };

  const save = async () => {
    if (days === null) return;
    setError(null);
    try {
      const saved = await api.savePair({
        id: existing?.id ?? null,
        name,
        source,
        replica,
        userRules: rules.split('\n').map((r) => r.trim()).filter(Boolean),
        trashDays: days,
        allowSameVolume: sameVolumeOk,
      });
      // Done: a retry after a failure below must not send these folders again.
      setExisting(saved);
      setSource(null);
      setReplica(null);
      if (second) {
        const withSecond = saved.replicas.length > 1
          ? await api.relinkReplica(saved.id, 1, second, sameVolumeOk)
          : await api.addReplica(saved.id, second, sameVolumeOk);
        setExisting(withSecond);
        setSecond(null);
      }
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

  const removeDisk = async (index: number) => {
    if (!existing) return;
    setRemoving(null);
    try {
      setExisting(await api.removeReplica(existing.id, index));
      // The slots shift: a folder picked for one now belongs to another disk.
      setReplica(null);
      setSecond(null);
      setAddingSecond(false);
      setSameVolumeOk(false);
      setAskSameVolume(false);
      setError(null);
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
  };

  const folderField = (label: string, value: string | null, set: (v: string) => void, current: string | null, extra?: ReactNode) => (
    <div className="panel" key={label}>
      <span>{label}</span>
      <div className="row">
        <span className="muted">{value ?? current ?? t('editor.notChosen')}</span>
        {extra}
        <button onClick={async () => {
          try {
            const f = await pickFolder(label);
            if (f) set(f);
          } catch (e) {
            setError(errorText(tx, lang, e));
          }
        }}>
          {current ? t('editor.change') : t('common.browse')}
        </button>
      </div>
    </div>
  );

  // Two backup disks (named Disk A and Disk B), or one that may get a second.
  const rotating = (existing?.replicas.length ?? 1) > 1 || addingSecond;
  const disks = rotating ? [0, 1] : [0];
  const currentOf = (slot: number) => {
    const side = existing?.replicas[slot]?.side;
    return side ? t('editor.current', { label: side.label, path: side.rel_path || '/' }) : null;
  };

  return (
    <main className="screen">
      <h1>{existing ? t('editor.editTitle') : t('editor.newTitle')}</h1>
      <label>
        {t('editor.name')}
        <input value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      {folderField(t('editor.source'), source, pickedFolder(setSource),
        existing ? t('editor.current', { label: existing.source.label, path: existing.source.rel_path || '/' }) : null)}
      {disks.map((slot) => folderField(
        rotating ? t('editor.diskFolder', { disk: diskName(t, slot) }) : t('editor.replica'),
        slot === 0 ? replica : second,
        pickedFolder(slot === 0 ? setReplica : setSecond),
        currentOf(slot),
        <>
          {existing && existing.replicas.length > 1 && (
            <button onClick={() => setRemoving(slot)}>{t('editor.removeDisk', { disk: diskName(t, slot) })}</button>
          )}
          {slot === 1 && addingSecond && existing?.replicas.length === 1 && (
            <button onClick={() => { setAddingSecond(false); setSecond(null); }}>{t('common.cancel')}</button>
          )}
        </>,
      ))}
      {!rotating && <button onClick={() => setAddingSecond(true)}>{t('editor.addDisk')}</button>}
      <p className="muted">{t('editor.advice')}</p>
      <label>
        {t('editor.rules')}
        <textarea rows={4} value={rules} onChange={(e) => setRules(e.target.value)} />
        <span className="muted">{t('editor.rulesHelp')}</span>
      </label>
      <p className="muted">{t('editor.builtins')}: {builtins.join(', ')}</p>
      <label>
        {t('editor.trashDays')}
        <input type="number" min={1} max={MAX_TRASH_DAYS} step={1} value={trashDays} aria-invalid={!daysValid}
          onChange={(e) => setTrashDays(e.target.value)} />
        {noDefaults && <span className="muted">{t('editor.noDefaults')}</span>}
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
      {removing !== null && existing?.replicas[removing] && (
        <Modal
          onClose={() => setRemoving(null)}
          title={t('editor.removeDisk', { disk: diskName(t, removing) })}
          actions={<>
            <button onClick={() => setRemoving(null)}>{t('common.cancel')}</button>
            <button className="primary" onClick={() => void removeDisk(removing)}>{t('editor.removeDiskConfirm')}</button>
          </>}
        >
          {t('editor.removeDiskBody', { disk: diskName(t, removing), label: existing.replicas[removing].side.label })}
        </Modal>
      )}
      {confirmDelete && existing && (
        <Modal
          onClose={() => setConfirmDelete(false)}
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
