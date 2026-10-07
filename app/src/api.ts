// Typed wrappers for the Tauri commands and events. Field names follow the
// Rust serde shapes: app-state types are camelCase, core types snake_case.
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open, save } from '@tauri-apps/plugin-dialog';

export type RelPath = string;
export type SideKind = 'Source' | 'Replica';

export interface AppError { code: string; params: Record<string, string> }

export interface Side { volume_id: string; rel_path: string; label: string }
export interface LastSync { at: string; applied: number; failed: number; stopped: boolean }
export interface Pair {
  id: string; name: string; source: Side; replica: Side; user_rules: string[];
  trash_days: number; last_sync: LastSync | null; last_scan_files: number | null;
}
export interface PairView { pair: Pair; sourceConnected: boolean; replicaConnected: boolean }
export interface PairInput {
  id: string | null; name: string; source: string | null; replica: string | null;
  userRules: string[]; trashDays: number; allowSameVolume: boolean;
}

export type LanguageSetting = 'auto' | 'en' | 'pt-PT';
export interface Settings { language: LanguageSetting; defaultTrashDays: number }
export interface SettingsView { settings: Settings; resolvedLanguage: 'en' | 'pt-PT' }

export type SkipReason =
  | { code: 'link' | 'fileVsFolder' | 'caseCollision' | 'notRegularFile' | 'deletedSinceScan' | 'changedSincePreview'
      | 'backOnSource' | 'alreadyGone' | 'noLongerInReplica' | 'folderNotEmpty' }
  | { code: 'unreadable'; side: SideKind; detail: string };
export type FailReason =
  | { code: 'inUse' | 'permissionDenied' | 'changedDuringCopy' | 'targetExists' | 'outsideReplica' | 'interrupted' }
  | { code: 'io'; detail: string };
export interface StopReason { code: 'cancelled' | 'replicaDisconnected' | 'sourceDisconnected' | 'replicaFull' }

export type MoveKind = { kind: 'file'; size: number; mtime_ns: number } | { kind: 'dir'; files: number; bytes: number };
export type Change =
  | { type: 'create'; path: RelPath; size: number; mtime_ns: number }
  | { type: 'update'; path: RelPath; size: number; mtime_ns: number; replica_newer: boolean }
  | { type: 'delete'; path: RelPath; size: number; mtime_ns: number }
  | { type: 'move'; from: RelPath; to: RelPath; kind: MoveKind }
  | { type: 'mkDir'; path: RelPath }
  | { type: 'rmDir'; path: RelPath }
  | { type: 'skipped'; path: RelPath; reason: SkipReason };

export interface Totals {
  creates: number; updates: number; moves: number; deletes: number; folders: number; skipped: number; bytes_to_copy: number;
}
export interface ScanStats { files: number; bytes: number; elapsed_ms: number }
export interface GuardWarning { affected: number; replica_files: number }
export interface PreviewSummary {
  pairId: string; pairName: string; totals: Totals; selected: Totals; selectedCount: number; actionableCount: number;
  isEmpty: boolean; guard: GuardWarning | null; guardConfirmed: boolean; shortfall: number | null;
  source: ScanStats; replica: ScanStats; deleteFolders: RelPath[];
}

export type Tick = 'on' | 'off' | 'mixed' | 'none';
export interface Counts { create: number; update: number; delete: number; moves: number; folders: number; skipped: number }
export interface RowChange { id: number; change: Change }
export interface NodeView {
  name: string; path: RelPath; isFolder: boolean; changes: RowChange[]; counts: Counts; bytesToCopy: number; tick: Tick;
}
/** The first `nodes.length` children of a folder; `total` counts them all. */
export interface ChildrenPage { nodes: NodeView[]; total: number }

export interface SideProgress { files: number; bytes: number; current: string }
export interface ScanProgress { source: SideProgress; replica: SideProgress; approxFiles: number | null }
export interface ApplyProgress {
  bytes_done: number; bytes_total: number; changes_done: number; changes_total: number; current: RelPath | null;
}
export type Outcome = { kind: 'applied' } | { kind: 'skipped'; reason: SkipReason } | { kind: 'failed'; reason: FailReason };
export interface ChangeResult { id: number; path: RelPath; outcome: Outcome }
export interface RunReport { results: ChangeResult[]; stopped: StopReason | null; trash_run: string | null }
export interface RunView {
  pairId: string; trashDays: number; report: RunReport; notDone: number; logPath: string | null; oldTrashRuns: string[]; warnings: AppError[];
}
export interface JobDone<T> { ok: T | null; error: AppError | null }

export interface TrashRunInfo { id: string; files: number; bytes: number; unrecorded: number }
export interface TrashItem { path: RelPath; size: number; mtime_ns: number; reason: 'deleted' | 'replaced' }
export interface TrashRunContents { items: TrashItem[]; unrecorded: RelPath[]; bytes: number }

export const api = {
  startupStatus: () => invoke<void>('startup_status'),
  dataFolder: () => invoke<string>('data_folder'),
  getSettings: () => invoke<SettingsView>('get_settings'),
  setSettings: (settings: Settings) => invoke<SettingsView>('set_settings', { settings }),
  builtinRules: () => invoke<string[]>('builtin_rules'),
  listPairs: () => invoke<PairView[]>('list_pairs'),
  savePair: (input: PairInput) => invoke<Pair>('save_pair', { input }),
  deletePair: (id: string) => invoke<void>('delete_pair', { id }),
  relink: (id: string, side: SideKind, folder: string) => invoke<Pair>('relink', { id, side, folder }),
  startScan: (id: string) => invoke<void>('start_scan', { id }),
  cancelScan: () => invoke<void>('cancel_scan'),
  previewSummary: () => invoke<PreviewSummary>('preview_summary'),
  treeChildren: (path: RelPath) => invoke<ChildrenPage>('tree_children', { path }),
  toggle: (path: RelPath) => invoke<PreviewSummary>('toggle', { path }),
  selectAll: (on: boolean) => invoke<PreviewSummary>('select_all', { on }),
  confirmWrongFolder: () => invoke<PreviewSummary>('confirm_wrong_folder'),
  savePreview: (file: string) => invoke<void>('save_preview', { file }),
  apply: () => invoke<void>('apply'),
  retryFailed: () => invoke<void>('retry_failed'),
  pause: () => invoke<void>('pause'),
  resume: () => invoke<void>('resume'),
  cancel: () => invoke<void>('cancel'),
  stopAndClose: () => invoke<void>('stop_and_close'),
  trashRuns: (id: string) => invoke<TrashRunInfo[]>('trash_runs', { id }),
  trashContents: (id: string, run: string) => invoke<TrashRunContents>('trash_contents', { id, run }),
  restore: (id: string, run: string, paths: RelPath[], replace: boolean) =>
    invoke<number>('restore', { id, run, paths, replace }),
  emptyRun: (id: string, run: string) => invoke<void>('empty_run', { id, run }),
  openLogsFolder: () => invoke<void>('open_logs_folder'),
  openDataFolder: () => invoke<void>('open_data_folder'),
  openPath: (path: string) => invoke<void>('open_path', { path }),
};

export type EventName = 'scan-progress' | 'scan-done' | 'apply-progress' | 'apply-done' | 'close-requested';

/** Subscribes to a Rust event; resolves to the unsubscribe function. */
export async function onEvent<T>(name: EventName, cb: (payload: T) => void): Promise<() => void> {
  return listen<T>(name, (e) => cb(e.payload));
}

/** `title` tells the user which folder to pick (the dialog has no other hint). */
export async function pickFolder(title?: string): Promise<string | null> {
  const r = await open({ directory: true, multiple: false, title });
  return typeof r === 'string' ? r : null;
}

export async function pickSaveFile(defaultName: string): Promise<string | null> {
  return save({ defaultPath: defaultName });
}
