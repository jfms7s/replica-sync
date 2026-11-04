import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { api, type ChildrenPage, type NodeView, type PreviewSummary, type SortOrder } from '../api';
import { formatBytes, formatCount } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';
import { changeDetail } from './changeText';

interface Row { node: NodeView; depth: number }
/** What is drawn: a node, or the note under a folder that has more children than Rust sends. */
type Line = { row: Row; index: number } | { more: number; path: string; depth: number };

/** Every row is this tall (`.tree [role="treeitem"]` in styles.css), so the virtualiser never measures. */
const ROW_HEIGHT = 24;

/** `''` and every folder above `path`: `a/b/c.txt` → `''`, `a`, `a/b`. */
function ancestors(path: string): string[] {
  const parts = path.split('/');
  return ['', ...parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join('/'))];
}

const isUnder = (p: string, folder: string) => p === folder || p.startsWith(`${folder}/`);

/** The Preview's folder tree: loads one folder at a time from Rust and draws only the rows in view. */
export default function Tree({ onSummary, expandRequest, refreshToken = 0 }: {
  onSummary: (s: PreviewSummary) => void;
  expandRequest: string[] | null;
  refreshToken?: number;
}) {
  const { t, tx, lang } = useT();
  const [children, setChildren] = useState<Map<string, ChildrenPage>>(new Map());
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  /** The row that keeps keyboard focus, by path, so collapsing or reloading never loses it. */
  const [focusPath, setFocusPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notShown, setNotShown] = useState(0);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const itemRefs = useRef(new Map<number, HTMLDivElement>());
  /** A row the keyboard moved to: focused once it is rendered. */
  const pendingFocus = useRef<number | null>(null);
  const expandedRef = useRef(expanded);
  expandedRef.current = expanded;
  const childrenRef = useRef(children);
  childrenRef.current = children;
  /** Each folder's sort order, for this session only; a folder not listed is by name. */
  const [orders, setOrders] = useState<Map<string, SortOrder>>(new Map());
  const ordersRef = useRef(orders);
  ordersRef.current = orders;
  const fetchPage = useCallback(
    async (p: string) => [p, await api.treeChildren(p, ordersRef.current.get(p) ?? 'name')] as const,
    [],
  );

  const load = useCallback(async (paths: string[]) => {
    const loaded = await Promise.all(paths.map(fetchPage));
    setChildren((prev) => {
      const next = new Map(prev);
      for (const [p, nodes] of loaded) next.set(p, nodes);
      return next;
    });
    return loaded;
  }, [fetchPage]);

  useEffect(() => {
    load(['']).catch((e) => setError(errorText(tx, lang, e)));
  }, [load, tx, lang]);

  const expand = useCallback(async (path: string) => {
    if (!childrenRef.current.has(path)) await load([path]);
    setExpanded((prev) => new Set(prev).add(path));
  }, [load]);

  useEffect(() => {
    if (!expandRequest) return;
    const paths = [...expandRequest].sort((a, b) => a.split('/').length - b.split('/').length);
    load(paths.filter((p) => !children.has(p)))
      .then((loaded) => {
        // A folder past its parent's first page is never drawn, so it is not
        // opened; the note says how many were left out. (A parent page not
        // loaded yet, the root while it loads, can't tell: open it.)
        const pages = new Map([...childrenRef.current, ...loaded]);
        const shown = new Set<string>(['']);
        for (const p of paths) {
          const parent = ancestors(p).at(-1)!;
          const page = pages.get(parent);
          if (shown.has(parent) && (!page || page.nodes.some((n) => n.path === p))) shown.add(p);
        }
        setNotShown(paths.filter((p) => !shown.has(p)).length);
        setExpanded((prev) => new Set([...prev, ...paths.filter((p) => shown.has(p))]));
      })
      .catch((e) => setError(errorText(tx, lang, e)));
    // Only react to a new request, not to every load.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [expandRequest]);

  /**
   * Reload the root and the open folders. Other folders cached before the reload began are dropped, so a later
   * expand refetches them; pages loaded meanwhile (an in-flight "Show them") are kept.
   */
  const refresh = useCallback(async () => {
    const paths = ['', ...expandedRef.current];
    const before = [...childrenRef.current.keys()];
    const loaded = await Promise.all(paths.map(fetchPage));
    setChildren((prev) => {
      const next = new Map(prev);
      for (const p of before) if (!paths.includes(p) && !expandedRef.current.has(p)) next.delete(p);
      for (const [p, page] of loaded) next.set(p, page);
      return next;
    });
  }, [fetchPage]);

  /**
   * After a toggle at `path` only its ancestors, itself and the folders under it can change. Those that are
   * open are refetched; those that are cached but closed are dropped (a later expand refetches them); every
   * other page is kept.
   */
  const refreshAfterToggle = useCallback(async (path: string) => {
    const open = expandedRef.current;
    const affected = [...childrenRef.current.keys()].filter((p) => p !== '' && isUnder(p, path));
    const refetch = [...ancestors(path), ...affected.filter((p) => open.has(p))];
    const drop = affected.filter((p) => !open.has(p));
    const loaded = await Promise.all(refetch.map(fetchPage));
    setChildren((prev) => {
      const next = new Map(prev);
      for (const p of drop) next.delete(p);
      for (const [p, page] of loaded) next.set(p, page);
      return next;
    });
  }, [fetchPage]);

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

  const collapse = useCallback((path: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      next.delete(path);
      return next;
    }), []);

  const toggle = useCallback(async (path: string) => {
    try {
      const summary = await api.toggle(path);
      onSummary(summary);
      await refreshAfterToggle(path);
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
  }, [onSummary, refreshAfterToggle, tx, lang]);

  /** Flip one folder between name order and largest first, and reload just that folder's page. */
  const setOrder = useCallback(async (path: string, order: SortOrder) => {
    const next = new Map(ordersRef.current).set(path, order);
    ordersRef.current = next;
    setOrders(next);
    try {
      await load([path]);
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
  }, [load, tx, lang]);

  const { rows, lines, lineOf } = useMemo(() => {
    const rows: Row[] = [];
    const lines: Line[] = [];
    const lineOf: number[] = [];
    const walk = (path: string, depth: number) => {
      const page = children.get(path);
      for (const node of page?.nodes ?? []) {
        const row = { node, depth };
        lineOf.push(lines.length);
        lines.push({ row, index: rows.length });
        rows.push(row);
        if (node.isFolder && expanded.has(node.path)) walk(node.path, depth + 1);
      }
      if (page && page.total > page.nodes.length) lines.push({ more: page.total - page.nodes.length, path, depth });
    };
    walk('', 1);
    return { rows, lines, lineOf };
  }, [children, expanded]);

  const virtualizer = useVirtualizer({
    count: lines.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 10,
  });

  // The keyboard moved focus to a row that may only now be rendered.
  useLayoutEffect(() => {
    const i = pendingFocus.current;
    if (i === null) return;
    const el = itemRefs.current.get(i);
    if (el) {
      pendingFocus.current = null;
      el.focus();
    }
  });

  // Handlers read the latest state through a ref, so the memoised rows get stable callbacks.
  const latest = useRef({ rows, lineOf, expanded, expand, collapse, toggle, virtualizer, setOrder });
  latest.current = { rows, lineOf, expanded, expand, collapse, toggle, virtualizer, setOrder };

  const onKey = useCallback((e: KeyboardEvent, i: number) => {
    const { rows, lineOf, expanded, expand, collapse, toggle, virtualizer } = latest.current;
    const row = rows[i];
    const move = (to: number) => {
      const j = Math.max(0, Math.min(rows.length - 1, to));
      setFocusPath(rows[j].node.path);
      pendingFocus.current = j;
      virtualizer.scrollToIndex(lineOf[j]);
      itemRefs.current.get(j)?.focus();
    };
    const parent = ancestors(row.node.path).at(-1)!;
    if (e.key === 'ArrowDown') move(i + 1);
    else if (e.key === 'ArrowUp') move(i - 1);
    else if (e.key === 'ArrowRight' && row.node.isFolder) void expand(row.node.path);
    else if (e.key === 'ArrowLeft' && row.node.isFolder && expanded.has(row.node.path)) collapse(row.node.path);
    else if (e.key === 'ArrowLeft' && parent !== '') move(rows.findIndex((r) => r.node.path === parent));
    else if (e.key === ' ' && row.node.tick !== 'none') void toggle(row.node.path);
    else return;
    e.preventDefault();
  }, []);

  const onOpenClick = useCallback((path: string) => {
    const { expanded, expand, collapse } = latest.current;
    if (expanded.has(path)) collapse(path);
    else void expand(path);
  }, []);

  const onToggle = useCallback((path: string) => void latest.current.toggle(path), []);

  const onSort = useCallback((path: string, order: SortOrder) => void latest.current.setOrder(path, order), []);

  const setItemRef = useCallback((i: number, el: HTMLDivElement | null) => {
    if (el) itemRefs.current.set(i, el);
    else itemRefs.current.delete(i);
  }, []);

  const items = virtualizer.getVirtualItems();
  // The focused row, or its nearest ancestor still shown once a collapse hid it.
  const focus = useMemo(() => {
    if (focusPath === null) return 0;
    const at = (p: string) => rows.findIndex((r) => r.node.path === p);
    const i = at(focusPath);
    if (i >= 0) return i;
    const parent = ancestors(focusPath).reverse().map(at).find((j) => j >= 0);
    return parent ?? 0;
  }, [rows, focusPath]);
  // Keep one row tabbable: the focused one, or the first in view once it has scrolled away.
  const rendered = items.flatMap((v) => {
    const line = lines[v.index];
    return line && 'row' in line ? [line.index] : [];
  });
  const tabbable = rendered.includes(focus) ? focus : rendered[0];
  const onFocusRow = useCallback((i: number) => setFocusPath(latest.current.rows[i]?.node.path ?? null), []);

  return (
    <>
    <div className="tree-tools">
      <SortButton path="" name="" order={orders.get('') ?? 'name'} onSort={onSort} />
    </div>
    <div className="tree panel" role="tree" aria-label={t('tree.label')} ref={scrollRef}>
      {error && <p className="error" role="alert">{error}</p>}
      {notShown > 0 && <p className="muted">{t('tree.notShown', { count: formatCount(lang, notShown) })}</p>}
      <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
        {items.map((v) => {
          const line = lines[v.index];
          const style = { position: 'absolute', top: 0, left: 0, right: 0, transform: `translateY(${v.start}px)` } as const;
          if ('more' in line) {
            return (
              <div key={`${line.path}\0more`} role="treeitem" aria-level={line.depth} aria-disabled tabIndex={-1}
                className="muted" style={{ ...style, paddingLeft: line.depth * 16 }}>
                {t(line.path === '' ? 'tree.moreRoot' : 'tree.more', { count: formatCount(lang, line.more) })}
              </div>
            );
          }
          const { row: { node, depth }, index } = line;
          return (
            <TreeRow
              key={node.path}
              node={node}
              depth={depth}
              index={index}
              top={v.start}
              expanded={node.isFolder && expanded.has(node.path)}
              tabbable={index === tabbable}
              order={orders.get(node.path) ?? 'name'}
              setItemRef={setItemRef}
              onKey={onKey}
              onFocusRow={onFocusRow}
              onOpenClick={onOpenClick}
              onToggle={onToggle}
              onSort={onSort}
            />
          );
        })}
      </div>
    </div>
    </>
  );
}

const TreeRow = memo(function TreeRow({ node, depth, index, top, expanded, tabbable, order, setItemRef, onKey, onFocusRow, onOpenClick, onToggle, onSort }: {
  node: NodeView;
  depth: number;
  index: number;
  top: number;
  expanded: boolean;
  tabbable: boolean;
  order: SortOrder;
  setItemRef: (i: number, el: HTMLDivElement | null) => void;
  onKey: (e: KeyboardEvent, i: number) => void;
  onFocusRow: (i: number) => void;
  onOpenClick: (path: string) => void;
  onToggle: (path: string) => void;
  onSort: (path: string, order: SortOrder) => void;
}) {
  const { t, tx, lang } = useT();
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
    <div
      ref={(el) => setItemRef(index, el)}
      role="treeitem"
      aria-label={node.name}
      aria-level={depth}
      aria-expanded={node.isFolder ? expanded : undefined}
      aria-checked={node.tick === 'mixed' ? 'mixed' : node.tick === 'on'}
      tabIndex={tabbable ? 0 : -1}
      style={{ position: 'absolute', top: 0, left: 0, right: 0, transform: `translateY(${top}px)`, paddingLeft: depth * 16 }}
      onKeyDown={(e) => onKey(e, index)}
      onFocus={() => onFocusRow(index)}
    >
      <input
        type="checkbox"
        aria-label={node.name}
        checked={node.tick === 'on'}
        disabled={node.tick === 'none'}
        ref={(el) => { if (el) el.indeterminate = node.tick === 'mixed'; }}
        onChange={() => onToggle(node.path)}
        tabIndex={-1}
      />
      {node.isFolder ? (
        <button className="link" tabIndex={-1} onClick={() => onOpenClick(node.path)}>
          {expanded ? '▾' : '▸'} {node.name}/
        </button>
      ) : (
        <span>{node.name}</span>
      )}
      {node.isFolder && expanded && <SortButton path={node.path} name={node.name} order={order} onSort={onSort} row />}
      {node.changes.map(({ id, change }) => (
        <span key={id} className="row">
          <span className={`badge ${change.type}`}>{tx(`change.${change.type}`)}</span>
          <span className="muted">{changeDetail(t, tx, lang, change)}</span>
        </span>
      ))}
      {node.isFolder && counts(node)}
      {node.bytesToCopy > 0 && <span className="muted">{formatBytes(lang, node.bytesToCopy)}</span>}
    </div>
  );
});

/** "Largest first" for one folder: pressed = that folder's children are by size, not by name. */
function SortButton({ path, name, order, onSort, row = false }: {
  path: string;
  name: string;
  order: SortOrder;
  onSort: (path: string, order: SortOrder) => void;
  /** Inside a tree row (kept out of the tab order like the row's other controls). */
  row?: boolean;
}) {
  const { t } = useT();
  return (
    <button
      type="button"
      className="link"
      tabIndex={row ? -1 : undefined}
      aria-pressed={order === 'size'}
      aria-label={row ? t('tree.sortBySizeIn', { name }) : undefined}
      onClick={() => onSort(path, order === 'size' ? 'name' : 'size')}
    >
      {t('tree.sortBySize')}
    </button>
  );
}
