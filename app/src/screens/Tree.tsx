import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from 'react';
import { api, type ChildrenPage, type NodeView, type PreviewSummary } from '../api';
import { formatBytes, formatCount } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';
import { changeDetail } from './changeText';

interface Row { node: NodeView; depth: number }
/** What is drawn: a node, or the note under a folder that has more children than Rust sends. */
type Line = { row: Row; index: number } | { more: number; path: string; depth: number };

/** The Preview's folder tree: loads one folder at a time from Rust. */
export default function Tree({ onSummary, expandRequest, refreshToken = 0 }: {
  onSummary: (s: PreviewSummary) => void;
  expandRequest: string[] | null;
  refreshToken?: number;
}) {
  const { t, tx, lang } = useT();
  const [children, setChildren] = useState<Map<string, ChildrenPage>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [focus, setFocus] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const itemRefs = useRef<(HTMLDivElement | null)[]>([]);
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;

  const load = useCallback(async (paths: string[]) => {
    const loaded = await Promise.all(paths.map(async (p) => [p, await api.treeChildren(p)] as const));
    setChildren((prev) => {
      const next = new Map(prev);
      for (const [p, nodes] of loaded) next.set(p, nodes);
      return next;
    });
  }, []);

  useEffect(() => {
    load(['']).catch((e) => setError(errorText(tx, lang, e)));
  }, [load, tx, lang]);

  const expand = useCallback(async (path: string) => {
    if (!children.has(path)) await load([path]);
    setExpanded((prev) => new Set(prev).add(path));
  }, [children, load]);

  useEffect(() => {
    if (!expandRequest) return;
    const paths = [...expandRequest].sort((a, b) => a.split('/').length - b.split('/').length);
    load(paths.filter((p) => !children.has(p)))
      .then(() => setExpanded((prev) => new Set([...prev, ...paths])))
      .catch((e) => setError(errorText(tx, lang, e)));
    // Only react to a new request, not to every load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandRequest]);

  /** Reload the root and the open folders; every other cached folder is dropped so a later expand refetches. */
  const refresh = useCallback(async () => {
    const paths = ['', ...expandedRef.current];
    const loaded = await Promise.all(paths.map(async (p) => [p, await api.treeChildren(p)] as const));
    setChildren(new Map(loaded));
  }, []);

  const firstRefresh = useRef(true);
  useEffect(() => {
    if (firstRefresh.current) {
      firstRefresh.current = false;
      return;
    }
    refresh().catch((e) => setError(errorText(tx, lang, e)));
    // Only react to a new token.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshToken]);

  const collapse = (path: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      next.delete(path);
      return next;
    });

  const toggle = async (path: string) => {
    try {
      const summary = await api.toggle(path);
      onSummary(summary);
      await refresh();
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
  };

  const rows: Row[] = [];
  const lines: Line[] = [];
  const walk = (path: string, depth: number) => {
    const page = children.get(path);
    for (const node of page?.nodes ?? []) {
      const row = { node, depth };
      lines.push({ row, index: rows.length });
      rows.push(row);
      if (node.isFolder && expanded.has(node.path)) walk(node.path, depth + 1);
    }
    if (page && page.total > page.nodes.length) lines.push({ more: page.total - page.nodes.length, path, depth });
  };
  walk('', 1);

  const onKey = (e: KeyboardEvent, i: number) => {
    const row = rows[i];
    const move = (to: number) => {
      const j = Math.max(0, Math.min(rows.length - 1, to));
      setFocus(j);
      itemRefs.current[j]?.focus();
    };
    if (e.key === 'ArrowDown') move(i + 1);
    else if (e.key === 'ArrowUp') move(i - 1);
    else if (e.key === 'ArrowRight' && row.node.isFolder) void expand(row.node.path);
    else if (e.key === 'ArrowLeft' && row.node.isFolder) collapse(row.node.path);
    else if (e.key === ' ' && row.node.tick !== 'none') void toggle(row.node.path);
    else return;
    e.preventDefault();
  };

  const counts = (n: NodeView) => {
    const c = n.counts;
    const parts: [number, string, string][] = [
      [c.create, 'create', `+${c.create}`],
      [c.update, 'update', `~${c.update}`],
      [c.delete, 'delete', `−${c.delete}`],
      [c.moves, 'move', `↷${c.moves}`],
      [c.folders, 'mkDir', `▣${c.folders}`],
      [c.skipped, 'skipped', `⊘${c.skipped}`],
    ];
    return parts.filter(([n]) => n > 0).map(([, cls, text]) => <span key={cls} className={`badge ${cls}`}>{text}</span>);
  };

  return (
    <div className="tree panel" role="tree" aria-label={t('tree.label')}>
      {error && <p className="error" role="alert">{error}</p>}
      {lines.map((line) => {
        if ('more' in line) {
          return (
            <div key={`${line.path}\0more`} role="treeitem" aria-level={line.depth} aria-disabled tabIndex={-1}
              className="muted" style={{ paddingLeft: line.depth * 16 }}>
              {t('tree.more', { count: formatCount(lang, line.more) })}
            </div>
          );
        }
        const { row: { node, depth }, index: i } = line;
        return (
        <div
          key={node.path}
          ref={(el) => { itemRefs.current[i] = el; }}
          role="treeitem"
          aria-label={node.name}
          aria-level={depth}
          aria-expanded={node.isFolder ? expanded.has(node.path) : undefined}
          aria-checked={node.tick === 'mixed' ? 'mixed' : node.tick === 'on'}
          tabIndex={i === focus ? 0 : -1}
          style={{ paddingLeft: depth * 16 }}
          onKeyDown={(e) => onKey(e, i)}
          onFocus={() => setFocus(i)}
        >
          <input
            type="checkbox"
            aria-label={node.name}
            checked={node.tick === 'on'}
            disabled={node.tick === 'none'}
            ref={(el) => { if (el) el.indeterminate = node.tick === 'mixed'; }}
            onChange={() => void toggle(node.path)}
            tabIndex={-1}
          />
          {node.isFolder ? (
            <button className="link" tabIndex={-1}
              onClick={() => (expanded.has(node.path) ? collapse(node.path) : void expand(node.path))}>
              {expanded.has(node.path) ? '▾' : '▸'} {node.name}/
            </button>
          ) : (
            <span>{node.name}</span>
          )}
          {node.changes.map(({ id, change }) => (
            <span key={id} className="row">
              <span className={`badge ${change.type}`}>{tx(`change.${change.type}`)}</span>
              <span className="muted">{changeDetail(t, tx, change)}</span>
            </span>
          ))}
          {node.isFolder && counts(node)}
          {node.bytesToCopy > 0 && <span className="muted">{formatBytes(lang, node.bytesToCopy)}</span>}
        </div>
        );
      })}
    </div>
  );
}
