import type { Change } from '../api';
import type { Key, Params } from '../i18n';
import { skipText } from '../reasons';

type T = (key: Key, params?: Params) => string;
type Tx = (key: string, params?: Params) => string;

/** One line explaining a change row, e.g. "source is newer" or "moved from Old/Trip". */
export function changeDetail(t: T, tx: Tx, c: Change): string {
  switch (c.type) {
    case 'create':
      return '';
    case 'update':
      return c.replica_newer ? t('detail.replicaNewer') : t('detail.sourceNewer');
    case 'delete':
      return t('detail.notOnSource');
    case 'move': {
      const sameName = c.from.toLowerCase() === c.to.toLowerCase();
      if (sameName) return t('detail.renamedFrom', { from: c.from });
      return c.kind.kind === 'dir' && c.kind.files > 0
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
