import type { Key, Params } from './i18n';

/** "Disk A" / "Disk B": a pair's backup disks are named by the order they were added in. */
export const diskName = (t: (key: Key, params?: Params) => string, index: number): string =>
  t('disk.name', { letter: String.fromCharCode(65 + index) });
