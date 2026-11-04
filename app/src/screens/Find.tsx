import { useEffect, useRef, useState } from 'react';
import { api, type FindPage, type NodeView, type PreviewSummary } from '../api';
import { formatBytes, formatCount } from '../format';
import { useT } from '../i18n';
import { errorText } from '../reasons';
import { ChangeLines, CountBadges } from './NodeParts';

/** Most matches asked for at once; the rest are counted, not listed. */
export const FIND_LIMIT = 200;
const DEBOUNCE_MS = 150;

/**
 * A lookup by file or folder name over the whole plan, past the Tree's row cap. It is not a filter: it never
 * changes what "Approve all" or the totals cover, and ticking a match toggles only that item.
 * `summary` is the current preview summary; a new one (any tick, anywhere) re-runs the search so ticks stay true.
 */
export default function Find({ summary, onToggled }: {
  summary: PreviewSummary;
  /** Called with the new summary after a match was ticked, so the Preview and its Tree catch up. */
  onToggled: (s: PreviewSummary) => void;
}) {
  const { t, tx, lang } = useT();
  const [query, setQuery] = useState('');
  const [page, setPage] = useState<FindPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Only the newest request may set the results. */
  const latest = useRef(0);

  useEffect(() => {
    const id = ++latest.current;
    if (query.trim() === '') return;
    const timer = setTimeout(() => {
      api.treeFind(query, FIND_LIMIT)
        .then((p) => { if (id === latest.current) { setPage(p); setError(null); } })
        .catch((e) => { if (id === latest.current) setError(errorText(tx, lang, e)); });
    }, DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [query, summary, tx, lang]);

  const tick = async (n: NodeView) => {
    try {
      onToggled(await api.toggle(n.path));
    } catch (e) {
      setError(errorText(tx, lang, e));
    }
  };

  return (
    <section className="find" aria-label={t('find.label')}>
      <input
        type="search"
        aria-label={t('find.label')}
        placeholder={t('find.placeholder')}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          if (e.target.value.trim() === '') setPage(null);
        }}
      />
      {error && <p className="error" role="alert">{error}</p>}
      {page && page.nodes.length === 0 && <p className="muted">{t('find.none')}</p>}
      {page && page.nodes.length > 0 && (
        <>
          <p className="muted">{t('find.hint')}</p>
          <ul className="find-results">
            {page.nodes.map((n) => (
              <li key={n.path} className="row">
                <input
                  type="checkbox"
                  aria-label={n.path}
                  checked={n.tick === 'on'}
                  disabled={n.tick === 'none'}
                  ref={(el) => { if (el) el.indeterminate = n.tick === 'mixed'; }}
                  onChange={() => void tick(n)}
                />
                <span>{n.name}{n.isFolder ? '/' : ''}</span>
                <span className="muted">{n.path.includes('/') ? n.path.slice(0, n.path.lastIndexOf('/')) : '/'}</span>
                <ChangeLines node={n} />
                {n.isFolder && <CountBadges node={n} />}
                {n.bytesToCopy > 0 && <span className="muted">{formatBytes(lang, n.bytesToCopy)}</span>}
              </li>
            ))}
          </ul>
          {page.totalMatches > page.nodes.length && (
            <p className="muted">{t('find.more', { count: formatCount(lang, page.totalMatches - page.nodes.length) })}</p>
          )}
        </>
      )}
    </section>
  );
}
