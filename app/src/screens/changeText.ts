import type { Change } from '../api';
import { formatBytes, formatDay } from '../format';
import type { Key, Lang, Params } from '../i18n';
import { skipText } from '../reasons';

type T = (key: Key, params?: Params) => string;
type Tx = (key: string, params?: Params) => string;

/** One line explaining a change row, e.g. "source is newer" or "moved from Old/Trip". */
export function changeDetail(t: T, tx: Tx, lang: Lang, c: Change): string {
  switch (c.type) {
    case 'create':
      return '';
    case 'update':
      return t(c.replica_newer ? 'detail.replicaNewer' : 'detail.sourceNewer', {
        from: formatDay(lang, c.replica_mtime_ns),
        to: formatDay(lang, c.mtime_ns),
        fromSize: formatBytes(lang, c.replica_size),
        toSize: formatBytes(lang, c.size),
      });
    case 'delete':
      return t('detail.notOnSource');
    case 'move': {
      const sameName = c.from.toLowerCase() === c.to.toLowerCase();
      if (sameName) return t('detail.renamedFrom', { from: c.from });
      return c.kind.kind === 'dir'
        ? t('detail.movedFolder', { from: c.from, files: c.kind.files })
        : t('detail.movedFrom', { from: c.from });
    }
    case 'mkDir':
    case 'rmDir':
      return '';
    case 'skipped':
      return skipText(tx, c.reason);
  }
}
