import type { NodeView } from '../api';
import { useT } from '../i18n';
import { changeDetail } from './changeText';

/** The node's own changes, one badge and one line of detail each. */
export function ChangeLines({ node }: { node: NodeView }) {
  const { t, tx, lang } = useT();
  return node.changes.map(({ id, change }) => (
    <span key={id} className="row">
      <span className={`badge ${change.type}`}>{tx(`change.${change.type}`)}</span>
      <span className="muted">{changeDetail(t, tx, lang, change)}</span>
    </span>
  ));
}

/** What a folder holds, as small count badges. */
export function CountBadges({ node }: { node: NodeView }) {
  const c = node.counts;
  const parts: [number, string, string][] = [
    [c.create, 'create', `+${c.create}`],
    [c.update, 'update', `~${c.update}`],
    [c.delete, 'delete', `−${c.delete}`],
    [c.moves, 'move', `↷${c.moves}`],
    [c.folders, 'mkDir', `▣${c.folders}`],
    [c.skipped, 'skipped', `⊘${c.skipped}`],
  ];
  return parts.filter(([n]) => n > 0).map(([, cls, text]) => <span key={cls} className={`badge ${cls}`}>{text}</span>);
}
