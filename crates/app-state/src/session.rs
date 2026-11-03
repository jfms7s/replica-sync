//! The app's state between commands: pairs, settings, the current preview and
//! the run in progress. Long jobs are split into begin (under the lock, fast),
//! run (no lock, on a worker thread) and finish (under the lock).

use crate::error::AppError;
use crate::paths::AppPaths;
use crate::settings::Settings;
use crate::tree::{ChildrenPage, PlanTree};
use replica_sync_core::execute::{
    Control, DRIVE_CHECK_EVERY, DriveCheck, ExecContext, Progress, RunReport, execute,
};
use replica_sync_core::model::CaseMode;
use replica_sync_core::model::{RelPath, SideKind};
use replica_sync_core::pairs::{self, LastSync, NewPair, Pair, PairStore, ResolveError, Resolved};
use replica_sync_core::plan::{
    ChangeId, GuardWarning, Totals, mass_change_guard, selected_totals, space_shortfall,
};
use replica_sync_core::reason::StopReason;
use replica_sync_core::rules::{BUILTIN_PATTERNS, SkipRules};
use replica_sync_core::runlog;
use replica_sync_core::safety::ReplicaRoot;
use replica_sync_core::session::{Prepared, ScanStats, SessionCounters, check_roots, prepare};
use replica_sync_core::trash::{self, OnConflict, TrashRunContents, TrashRunInfo};
use replica_sync_core::volume::{self, Volumes};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Rows one folder may send to the Preview; a folder with more says how many
/// are not shown (its checkbox still includes or excludes them all).
pub const CHILDREN_LIMIT: usize = 2000;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PairView {
    pub pair: Pair,
    pub source_connected: bool,
    pub replica_connected: bool,
}

/// From the pair editor. `source`/`replica` are `None` when editing and the
/// folder is unchanged.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairInput {
    pub id: Option<String>,
    pub name: String,
    pub source: Option<PathBuf>,
    pub replica: Option<PathBuf>,
    pub user_rules: Vec<String>,
    pub trash_days: u32,
    pub allow_same_volume: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSummary {
    pub pair_id: String,
    pub pair_name: String,
    pub totals: Totals,
    pub selected: Totals,
    pub selected_count: usize,
    pub actionable_count: usize,
    pub is_empty: bool,
    pub guard: Option<GuardWarning>,
    pub guard_confirmed: bool,
    pub shortfall: Option<u64>,
    pub source: ScanStats,
    pub replica: ScanStats,
    /// Problems during the scan that did not stop it (store not written, free
    /// space unknown).
    pub warnings: Vec<AppError>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub pair_id: String,
    pub trash_days: u32,
    pub report: RunReport,
    /// Approved changes the run never reached (it stopped first).
    pub not_done: usize,
    pub log_path: Option<PathBuf>,
    pub old_trash_runs: Vec<String>,
    /// Problems after the run that did not stop it (log or store not written).
    pub warnings: Vec<AppError>,
}

struct Current {
    pair_id: String,
    resolved: Resolved,
    prepared: Arc<Prepared>,
    tree: PlanTree,
    selection: HashSet<ChangeId>,
    first_sync: bool,
    /// Free space on the replica at scan time; `None` when it couldn't be read.
    free_space: Option<u64>,
    /// Warnings from the scan itself (store not saved), for every summary.
    warnings: Vec<AppError>,
    /// How many removals the user confirmed; a selection that removes more
    /// asks again.
    guard_confirmed_for: Option<u64>,
    delete_folders: Vec<RelPath>,
    last_report: Option<RunReport>,
}

pub struct ScanJob {
    pair_id: String,
    resolved: Resolved,
    rules: SkipRules,
    case: CaseMode,
    pub counters: Arc<SessionCounters>,
    pub approx_files: Option<u64>,
}

impl ScanJob {
    pub fn run(&self) -> Result<Prepared, AppError> {
        prepare(
            &self.resolved.source_root,
            &self.resolved.replica_root,
            &self.rules,
            self.case,
            &self.counters,
        )
        .map_err(AppError::from)
    }
}

pub struct ApplyJob {
    pair_id: String,
    prepared: Arc<Prepared>,
    approved: HashSet<ChangeId>,
    source_root: PathBuf,
    replica: ReplicaRoot,
    control: Arc<Control>,
    stamp: String,
    /// The pair as it was at begin, and where its drives were: during the run
    /// both drives are re-checked against it (see [`DriveCheck`]).
    pair: Pair,
    resolved: Resolved,
    drive_check_every: Duration,
}

impl ApplyJob {
    pub fn run(&self, on_progress: &mut dyn FnMut(&Progress)) -> RunReport {
        self.run_on(&volume::system(), on_progress)
    }

    /// [`run`](Self::run) against the given drives (tests pass fakes).
    fn run_on(&self, volumes: &dyn Volumes, on_progress: &mut dyn FnMut(&Progress)) -> RunReport {
        // A different or missing drive stops the run. A one-off lookup error
        // is let through; a second one in a row stops it like a missing drive.
        let io_errors = Cell::new(0u32);
        let check = || {
            let sides = [
                (
                    pairs::verify_source(&self.pair, &self.resolved, volumes),
                    StopReason::SourceDisconnected,
                ),
                (
                    pairs::verify_replica(&self.pair, &self.resolved, volumes),
                    StopReason::ReplicaDisconnected,
                ),
            ];
            let mut lookup_failed = None;
            for (result, stop) in sides {
                match result {
                    Ok(()) => {}
                    Err(ResolveError::Io(_)) => lookup_failed = lookup_failed.or(Some(stop)),
                    Err(_) => return Err(stop),
                }
            }
            match lookup_failed {
                None => {
                    io_errors.set(0);
                    Ok(())
                }
                Some(stop) => {
                    io_errors.set(io_errors.get() + 1);
                    if io_errors.get() >= 2 {
                        Err(stop)
                    } else {
                        Ok(())
                    }
                }
            }
        };
        let ctx = ExecContext {
            source_root: &self.source_root,
            replica: &self.replica,
            control: &self.control,
            trash_stamp: self.stamp.clone(),
            drive_check: Some(DriveCheck {
                check: &check,
                every: self.drive_check_every,
            }),
        };
        execute(&self.prepared.plan, &self.approved, &ctx, on_progress)
    }
}

pub struct Session {
    paths: AppPaths,
    store: PairStore,
    settings: Settings,
    current: Option<Current>,
    scan_counters: Option<Arc<SessionCounters>>,
    control: Option<Arc<Control>>,
    applying: bool,
    scanning: bool,
}

fn connected(side: &pairs::Side, volumes: &dyn Volumes) -> bool {
    volumes.find(&side.volume_id).ok().flatten().is_some()
}

impl Session {
    /// Fails with `store.unreadable` if `pairs.json` exists but can't be read;
    /// in that case nothing ever writes to it.
    pub fn open(paths: AppPaths) -> Result<Session, AppError> {
        let file = paths.pairs_file();
        let store = PairStore::load(&file).map_err(|e| {
            AppError::new("store.unreadable")
                .with("path", file.display())
                .with("detail", e)
        })?;
        let settings = Settings::load(&paths.settings_file());
        Ok(Session {
            paths,
            store,
            settings,
            current: None,
            scan_counters: None,
            control: None,
            applying: false,
            scanning: false,
        })
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn set_settings(&mut self, settings: Settings) -> Result<(), AppError> {
        check_trash_days(settings.default_trash_days)?;
        settings.save(&self.paths.settings_file())?;
        self.settings = settings;
        Ok(())
    }

    pub fn is_applying(&self) -> bool {
        self.applying
    }

    pub fn builtin_rules() -> Vec<&'static str> {
        BUILTIN_PATTERNS.to_vec()
    }

    fn pair(&self, id: &str) -> Result<&Pair, AppError> {
        self.store
            .get(id)
            .ok_or_else(|| AppError::new("pair.notFound").with("id", id))
    }

    fn ensure_idle(&self) -> Result<(), AppError> {
        if self.applying || self.scanning {
            Err(AppError::new("apply.busy"))
        } else {
            Ok(())
        }
    }

    pub fn pair_views(&self, volumes: &dyn Volumes) -> Vec<PairView> {
        self.store
            .pairs
            .iter()
            .map(|p| PairView {
                pair: p.clone(),
                source_connected: connected(&p.source, volumes),
                replica_connected: connected(&p.replica, volumes),
            })
            .collect()
    }

    pub fn save_pair(&mut self, input: PairInput, volumes: &dyn Volumes) -> Result<Pair, AppError> {
        self.ensure_idle()?;
        if input.name.trim().is_empty() {
            return Err(AppError::new("pair.nameRequired"));
        }
        check_trash_days(input.trash_days)?;
        SkipRules::new(&input.user_rules)?;
        let pair = match &input.id {
            None => {
                let (Some(source), Some(replica)) = (&input.source, &input.replica) else {
                    return Err(AppError::new("pair.foldersRequired"));
                };
                pairs::create_pair(
                    NewPair {
                        name: &input.name,
                        source,
                        replica,
                        user_rules: input.user_rules.clone(),
                        trash_days: input.trash_days,
                        allow_same_volume: input.allow_same_volume,
                    },
                    volumes,
                )?
            }
            Some(id) => {
                let mut pair = self.pair(id)?.clone();
                // With both folders changing, the source is checked against the
                // new replica in the second step, not against the old one here.
                if let Some(folder) = &input.source {
                    let allow = input.allow_same_volume || input.replica.is_some();
                    pairs::relink(&mut pair, SideKind::Source, folder, allow, volumes)?;
                }
                if let Some(folder) = &input.replica {
                    let allow = input.allow_same_volume;
                    pairs::relink(&mut pair, SideKind::Replica, folder, allow, volumes)?;
                }
                let moved = input.source.is_some() || input.replica.is_some();
                if moved
                    && pair.source.volume_id == pair.replica.volume_id
                    && !input.allow_same_volume
                {
                    return Err(AppError::new("pair.sameVolume"));
                }
                pair.name = input.name.trim().to_owned();
                pair.user_rules = input.user_rules.clone();
                pair.trash_days = input.trash_days;
                pair
            }
        };
        self.store.upsert(pair.clone());
        self.store.save()?;
        if self.current.as_ref().is_some_and(|c| c.pair_id == pair.id) {
            self.current = None; // the preview no longer matches the pair
        }
        Ok(pair)
    }

    pub fn delete_pair(&mut self, id: &str) -> Result<(), AppError> {
        self.ensure_idle()?;
        if !self.store.remove(id) {
            return Err(AppError::new("pair.notFound").with("id", id));
        }
        self.store.save()?;
        if self.current.as_ref().is_some_and(|c| c.pair_id == id) {
            self.current = None;
        }
        Ok(())
    }

    pub fn relink(
        &mut self,
        id: &str,
        side: SideKind,
        folder: &Path,
        allow_same_volume: bool,
        volumes: &dyn Volumes,
    ) -> Result<Pair, AppError> {
        self.ensure_idle()?;
        let mut pair = self.pair(id)?.clone();
        pairs::relink(&mut pair, side, folder, allow_same_volume, volumes)?;
        self.store.upsert(pair.clone());
        self.store.save()?;
        if self.current.as_ref().is_some_and(|c| c.pair_id == pair.id) {
            self.current = None; // the preview no longer matches the pair
        }
        Ok(pair)
    }

    pub fn begin_scan(&mut self, id: &str, volumes: &dyn Volumes) -> Result<ScanJob, AppError> {
        self.ensure_idle()?;
        let pair = self.pair(id)?.clone();
        let resolved = pairs::resolve(&pair, volumes)?;
        let rules = pair.rules()?;
        // Before the case probe, which writes into the replica.
        check_roots(&resolved.source_root, &resolved.replica_root)?;
        let case = volume::case_mode(&resolved.replica_root)?;
        self.current = None;
        let counters = Arc::new(SessionCounters::default());
        self.scan_counters = Some(counters.clone());
        self.scanning = true;
        Ok(ScanJob {
            pair_id: pair.id,
            resolved,
            rules,
            case,
            counters,
            approx_files: pair.last_scan_files,
        })
    }

    pub fn cancel_scan(&self) {
        if let Some(c) = &self.scan_counters {
            c.cancel();
        }
    }

    pub fn finish_scan(
        &mut self,
        job: ScanJob,
        result: Result<Prepared, AppError>,
        _volumes: &dyn Volumes,
    ) -> Result<PreviewSummary, AppError> {
        let current_scan = self
            .scan_counters
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(c, &job.counters));
        if !current_scan {
            return Err(AppError::new("scan.cancelled"));
        }
        self.scanning = false;
        self.scan_counters = None;
        let prepared = result?;
        let mut warnings = Vec::new();
        // Unknown free space skips the shortfall check rather than the preview
        // (the copy itself still fails cleanly on a full drive); `summary`
        // warns while it stays unknown.
        let free_space = volume::free_space(&job.resolved.replica_root).ok();
        let pair = self
            .store
            .pairs
            .iter_mut()
            .find(|p| p.id == job.pair_id)
            .ok_or_else(|| AppError::new("pair.notFound").with("id", &job.pair_id))?;
        let first_sync = pair.is_first_sync();
        pair.last_scan_files = Some(prepared.source.files);
        if let Err(e) = self.store.save() {
            warnings.push(e.into());
        }
        let tree = PlanTree::new(&prepared.plan);
        let delete_folders = tree.folders_with_deletes(&prepared.plan);
        let selection = prepared.plan.actionable_ids();
        self.current = Some(Current {
            pair_id: job.pair_id,
            resolved: job.resolved,
            prepared: Arc::new(prepared),
            tree,
            selection,
            first_sync,
            free_space,
            warnings,
            guard_confirmed_for: None,
            delete_folders,
            last_report: None,
        });
        self.summary()
    }

    fn current(&self) -> Result<&Current, AppError> {
        self.current
            .as_ref()
            .ok_or_else(|| AppError::new("apply.noPlan"))
    }

    fn current_mut(&mut self) -> Result<&mut Current, AppError> {
        self.current
            .as_mut()
            .ok_or_else(|| AppError::new("apply.noPlan"))
    }

    pub fn summary(&self) -> Result<PreviewSummary, AppError> {
        let c = self.current()?;
        let pair = self.pair(&c.pair_id)?;
        let plan = &c.prepared.plan;
        let selected = selected_totals(plan, &c.selection);
        let mut warnings = c.warnings.clone();
        let free_space = match volume::free_space(&c.resolved.replica_root) {
            Ok(free) => Some(free),
            Err(e) => {
                if c.free_space.is_none() {
                    warnings.push(e.into());
                }
                c.free_space
            }
        };
        let shortfall = free_space.and_then(|free| space_shortfall(selected.bytes_to_copy, free));
        let guard = mass_change_guard(plan, &c.selection, c.prepared.replica.files, c.first_sync);
        Ok(PreviewSummary {
            pair_id: c.pair_id.clone(),
            pair_name: pair.name.clone(),
            totals: plan.totals.clone(),
            selected,
            selected_count: c.selection.len(),
            actionable_count: plan.changes.len() - plan.totals.skipped,
            is_empty: plan.is_empty(),
            guard_confirmed: guard
                .as_ref()
                .zip(c.guard_confirmed_for)
                .is_some_and(|(g, n)| g.affected <= n),
            guard,
            shortfall,
            source: c.prepared.source.clone(),
            replica: c.prepared.replica.clone(),
            warnings,
        })
    }

    /// The first `limit` folders holding a planned delete (every ancestor of
    /// every `Delete`, sorted), for the Preview's "Show them". Kept out of
    /// [`PreviewSummary`] so a huge delete is not re-sent on every click.
    pub fn delete_folders(&self, limit: usize) -> Result<Vec<RelPath>, AppError> {
        Ok(self
            .current()?
            .delete_folders
            .iter()
            .take(limit)
            .cloned()
            .collect())
    }

    /// A folder's children for the Preview, at most [`CHILDREN_LIMIT`] of them.
    pub fn children(&self, folder: &RelPath) -> Result<ChildrenPage, AppError> {
        let c = self.current()?;
        Ok(c.tree
            .children(&c.prepared.plan, folder, &c.selection, CHILDREN_LIMIT))
    }

    pub fn toggle(&mut self, path: &RelPath) -> Result<PreviewSummary, AppError> {
        self.ensure_idle()?;
        let c = self.current_mut()?;
        c.tree.toggle(&c.prepared.plan, path, &mut c.selection);
        self.summary()
    }

    pub fn select_all(&mut self, on: bool) -> Result<PreviewSummary, AppError> {
        self.ensure_idle()?;
        let c = self.current_mut()?;
        c.selection = if on {
            c.prepared.plan.actionable_ids()
        } else {
            HashSet::new()
        };
        self.summary()
    }

    pub fn confirm_guard(&mut self) -> Result<PreviewSummary, AppError> {
        self.ensure_idle()?;
        let affected = self.summary()?.guard.map(|g| g.affected);
        self.current_mut()?.guard_confirmed_for = affected;
        self.summary()
    }

    pub fn save_preview(&self, file: &Path) -> Result<(), AppError> {
        let c = self.current()?;
        std::fs::write(file, runlog::render_plan(&c.prepared.plan, &c.selection))?;
        Ok(())
    }

    pub fn begin_apply(&mut self, volumes: &dyn Volumes) -> Result<ApplyJob, AppError> {
        self.ensure_idle()?;
        let c = self.current()?;
        if c.last_report.is_some() {
            return Err(AppError::new("apply.noPlan"));
        }
        if c.selection.is_empty() {
            return Err(AppError::new("apply.nothingSelected"));
        }
        let approved = c.selection.clone();
        self.verify_drives(volumes)?;
        let s = self.summary()?;
        if s.guard.is_some() && !s.guard_confirmed {
            return Err(AppError::new("apply.wrongFolderUnconfirmed"));
        }
        if let Some(bytes) = s.shortfall {
            return Err(AppError::new("apply.spaceShortfall").with("bytes", bytes));
        }
        self.start_job(approved)
    }

    /// Re-runs only the changes that failed, on the same plan (ids belong to it).
    pub fn begin_retry(&mut self, volumes: &dyn Volumes) -> Result<ApplyJob, AppError> {
        self.ensure_idle()?;
        let ids = self
            .current()?
            .last_report
            .as_ref()
            .map(RunReport::failed_ids)
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(AppError::new("apply.nothingToRetry"));
        }
        self.verify_drives(volumes)?;
        self.start_job(ids)
    }

    fn verify_drives(&self, volumes: &dyn Volumes) -> Result<(), AppError> {
        let c = self.current()?;
        let pair = self.pair(&c.pair_id)?;
        let now = pairs::resolve(pair, volumes)?;
        // Name the side that moved; the replica when both did.
        let moved = if now.replica_root != c.resolved.replica_root {
            Some(&now.replica_root)
        } else if now.source_root != c.resolved.source_root {
            Some(&now.source_root)
        } else {
            None
        };
        if let Some(root) = moved {
            return Err(AppError::new("drive.wrongVolume").with("path", root.display()));
        }
        pairs::verify_source(pair, &c.resolved, volumes)?;
        pairs::verify_replica(pair, &c.resolved, volumes)?;
        Ok(())
    }

    fn start_job(&mut self, approved: HashSet<ChangeId>) -> Result<ApplyJob, AppError> {
        let c = self.current()?;
        let pair = self.pair(&c.pair_id)?.clone();
        let replica = ReplicaRoot::new(&c.resolved.replica_root)?;
        let control = Arc::new(Control::default());
        let job = ApplyJob {
            pair_id: c.pair_id.clone(),
            prepared: c.prepared.clone(),
            approved,
            source_root: c.resolved.source_root.clone(),
            replica,
            control: control.clone(),
            stamp: trash::new_run_stamp(),
            pair,
            resolved: c.resolved.clone(),
            drive_check_every: DRIVE_CHECK_EVERY,
        };
        self.control = Some(control);
        self.applying = true;
        Ok(job)
    }

    pub fn pause(&self) {
        if let Some(c) = &self.control {
            c.pause();
        }
    }

    pub fn resume(&self) {
        if let Some(c) = &self.control {
            c.resume();
        }
    }

    pub fn cancel_apply(&self) {
        if let Some(c) = &self.control {
            c.cancel();
        }
    }

    pub fn finish_apply(&mut self, job: ApplyJob, report: RunReport) -> Result<RunView, AppError> {
        self.applying = false;
        self.control = None;
        let pair_id = job.pair_id.clone();
        let replica_root = job.replica.path().to_path_buf();
        let (name, trash_days) = {
            let p = self.pair(&pair_id)?;
            (p.name.clone(), p.trash_days)
        };
        let mut warnings = Vec::new();
        let logs = self.paths.logs_dir();
        let text = runlog::render_run(&name, &job.prepared, &job.approved, &report);
        let log_path = match runlog::write_log(&logs, &name, &job.stamp, &text) {
            Ok(p) => Some(p),
            Err(e) => {
                warnings.push(e.into());
                None
            }
        };
        if let Err(e) = runlog::prune(&logs, runlog::LOG_RETENTION, SystemTime::now()) {
            warnings.push(e.into());
        }
        if let Some(p) = self.store.pairs.iter_mut().find(|p| p.id == pair_id) {
            p.last_sync = Some(LastSync {
                at: chrono::Local::now().to_rfc3339(),
                applied: report.applied(),
                failed: report.failed(),
                stopped: report.stopped.is_some(),
            });
        }
        if let Err(e) = self.store.save() {
            warnings.push(e.into());
        }
        // Never offer to empty the run this apply just made.
        let old_trash_runs: Vec<String> = match trash::runs_older_than(
            &replica_root,
            trash_days,
            chrono::Local::now().naive_local(),
        ) {
            Ok(runs) => runs
                .into_iter()
                .filter(|id| report.trash_run.as_ref() != Some(id))
                .collect(),
            Err(e) => {
                warnings.push(e.into());
                Vec::new()
            }
        };
        if let Some(c) = self
            .current
            .as_mut()
            .filter(|c| Arc::ptr_eq(&c.prepared, &job.prepared))
        {
            c.last_report = Some(report.clone());
        }
        let not_done = job.approved.len().saturating_sub(report.results.len());
        Ok(RunView {
            pair_id,
            trash_days,
            not_done,
            report,
            log_path,
            old_trash_runs,
            warnings,
        })
    }

    fn replica_of(&self, id: &str, volumes: &dyn Volumes) -> Result<PathBuf, AppError> {
        Ok(pairs::resolve_replica(self.pair(id)?, volumes)?)
    }

    pub fn trash_runs(
        &self,
        id: &str,
        volumes: &dyn Volumes,
    ) -> Result<Vec<TrashRunInfo>, AppError> {
        Ok(trash::list_runs(&self.replica_of(id, volumes)?)?)
    }

    pub fn trash_contents(
        &self,
        id: &str,
        run: &str,
        volumes: &dyn Volumes,
    ) -> Result<TrashRunContents, AppError> {
        Ok(trash::run_contents(&self.replica_of(id, volumes)?, run)?)
    }

    pub fn restore(
        &mut self,
        id: &str,
        run: &str,
        paths: &[RelPath],
        replace: bool,
        volumes: &dyn Volumes,
    ) -> Result<usize, AppError> {
        self.ensure_idle()?;
        let root = self.replica_of(id, volumes)?;
        let mode = if replace {
            OnConflict::TrashExisting
        } else {
            OnConflict::Refuse
        };
        Ok(trash::restore(&root, run, paths, mode)?)
    }

    /// The `files` folder of one trash run, for the file manager (the UI sends
    /// ids only, never a path).
    pub fn trash_files_folder(
        &self,
        id: &str,
        run: &str,
        volumes: &dyn Volumes,
    ) -> Result<PathBuf, AppError> {
        Ok(trash::files_dir(&self.replica_of(id, volumes)?, run)?)
    }

    pub fn empty_run(
        &mut self,
        id: &str,
        run: &str,
        volumes: &dyn Volumes,
    ) -> Result<(), AppError> {
        self.ensure_idle()?;
        Ok(trash::empty_run(&self.replica_of(id, volumes)?, run)?)
    }
}

/// The longest trash age (100 years); the UI caps its fields at the same value.
pub const MAX_TRASH_DAYS: u32 = 36_500;

/// An age of 0 would offer to empty the trash run the next apply makes; a
/// larger one than [`MAX_TRASH_DAYS`] is a typo.
fn check_trash_days(days: u32) -> Result<(), AppError> {
    if days == 0 || days > MAX_TRASH_DAYS {
        return Err(AppError::new("pair.badTrashDays").with("max", MAX_TRASH_DAYS));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FakeVolumes, write};
    use std::fs;

    struct World {
        t: tempfile::TempDir,
        v: FakeVolumes,
        s: Session,
        id: String,
    }

    impl World {
        fn src(&self) -> PathBuf {
            self.t.path().join("diskA/Photos")
        }
        fn rep(&self) -> PathBuf {
            self.t.path().join("diskB/Backup")
        }
        fn scan(&mut self) -> PreviewSummary {
            let job = self.s.begin_scan(&self.id, &self.v).unwrap();
            let r = job.run();
            self.s.finish_scan(job, r, &self.v).unwrap()
        }
        fn apply(&mut self) -> RunView {
            let job = self.s.begin_apply(&self.v).unwrap();
            let report = job.run_on(&self.v, &mut |_| {});
            self.s.finish_apply(job, report).unwrap()
        }
    }

    fn world() -> World {
        let t = tempfile::tempdir().unwrap();
        fs::create_dir_all(t.path().join("diskA/Photos")).unwrap();
        fs::create_dir_all(t.path().join("diskB/Backup")).unwrap();
        let v = FakeVolumes::default();
        v.mount("A", &t.path().join("diskA"));
        v.mount("B", &t.path().join("diskB"));
        let mut s = Session::open(AppPaths::new(t.path().join("data"))).unwrap();
        let input = PairInput {
            id: None,
            name: "Photos".into(),
            source: Some(t.path().join("diskA/Photos")),
            replica: Some(t.path().join("diskB/Backup")),
            user_rules: vec![],
            trash_days: 30,
            allow_same_volume: false,
        };
        let id = s.save_pair(input, &v).unwrap().id;
        World { t, v, s, id }
    }

    #[test]
    fn full_flow_copies_logs_and_records_last_sync() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        write(&w.src(), "b/c.txt", b"c");
        let s = w.scan();
        assert_eq!(
            (s.totals.creates, s.actionable_count, s.selected_count),
            (2, 2, 2)
        );
        let root = w.s.children(&RelPath::root()).unwrap().nodes;
        assert_eq!(
            root.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(),
            vec!["a.txt", "b"]
        );
        let run = w.apply();
        assert_eq!(run.report.applied(), 2);
        assert!(w.rep().join("b/c.txt").exists());
        assert!(run.log_path.as_ref().is_some_and(|p| p.exists()));
        assert!(w.s.pair_views(&w.v)[0].pair.last_sync.is_some());
        assert!(w.scan().is_empty);
    }

    #[test]
    fn delete_folders_returns_at_most_the_limit_in_order() {
        let mut w = world();
        for i in 0..120 {
            write(&w.rep(), &format!("d{i:03}/x.txt"), b"x");
        }
        w.scan();
        let got = w.s.delete_folders(50).unwrap();
        assert_eq!(got.len(), 50);
        assert_eq!(got[0].as_str(), "d000");
        assert_eq!(got[49].as_str(), "d049");
        assert_eq!(w.s.delete_folders(500).unwrap().len(), 120);
    }

    #[test]
    fn toggling_a_folder_leaves_it_out_of_the_apply() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        write(&w.src(), "b/c.txt", b"c");
        w.scan();
        let s = w.s.toggle(&RelPath::new("b").unwrap()).unwrap();
        assert_eq!(s.selected_count, 1);
        w.apply();
        assert!(w.rep().join("a.txt").exists() && !w.rep().join("b").exists());
    }

    #[test]
    fn first_sync_guard_blocks_apply_until_confirmed() {
        let mut w = world();
        for i in 0..5 {
            write(&w.rep(), &format!("other{i}.txt"), b"x");
        }
        write(&w.src(), "a.txt", b"a");
        let s = w.scan();
        assert!(s.guard.as_ref().is_some_and(|g| g.first_sync) && !s.guard_confirmed);
        assert_eq!(
            w.s.begin_apply(&w.v).err().unwrap().code,
            "apply.wrongFolderUnconfirmed"
        );
        assert!(!w.s.is_applying());
        w.s.confirm_guard().unwrap();
        let run = w.apply();
        assert_eq!(run.report.failed(), 0);
    }

    #[test]
    fn later_sync_removing_most_of_the_backup_needs_confirmation() {
        let mut w = world();
        for i in 0..4 {
            write(&w.src(), &format!("f{i}.txt"), b"x");
        }
        w.scan();
        w.apply();
        // Small later changes don't trip the guard.
        fs::remove_file(w.src().join("f0.txt")).unwrap();
        let s = w.scan();
        assert_eq!((s.totals.deletes, s.guard.is_none()), (1, true));
        // The source is emptied (moved away, or another folder at its path).
        for i in 1..4 {
            fs::remove_file(w.src().join(format!("f{i}.txt"))).unwrap();
        }
        let s = w.scan();
        let g = s.guard.as_ref().expect("guard on a later sync");
        assert_eq!((g.affected, g.replica_files, g.first_sync), (4, 4, false));
        assert_eq!(
            w.s.begin_apply(&w.v).err().unwrap().code,
            "apply.wrongFolderUnconfirmed"
        );
        assert!(w.rep().join("f1.txt").exists());
        // A fresh scan forgets an earlier confirmation.
        w.s.confirm_guard().unwrap();
        assert!(!w.scan().guard_confirmed);
        // Confirming a smaller selection doesn't cover a bigger one.
        w.s.toggle(&RelPath::new("f1.txt").unwrap()).unwrap();
        assert!(w.s.confirm_guard().unwrap().guard_confirmed);
        let s = w.s.select_all(true).unwrap();
        assert!(s.guard.is_some() && !s.guard_confirmed);
        assert_eq!(
            w.s.begin_apply(&w.v).err().unwrap().code,
            "apply.wrongFolderUnconfirmed"
        );
        w.s.confirm_guard().unwrap();
        assert_eq!(w.apply().report.failed(), 0);
        assert!(!w.rep().join("f1.txt").exists());
    }

    #[test]
    fn a_source_moved_after_preview_is_named_in_the_wrong_drive_error() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        let elsewhere = w.t.path().join("diskA2");
        fs::create_dir_all(elsewhere.join("Photos")).unwrap();
        w.v.mount("A", &elsewhere);
        let e = w.s.begin_apply(&w.v).err().unwrap();
        assert_eq!(e.code, "drive.wrongVolume");
        assert!(e.params["path"].contains("diskA2"), "{e:?}");
    }

    /// A data folder that can't be written keeps the preview, with a warning.
    #[cfg(unix)]
    #[test]
    fn a_store_that_cannot_be_saved_still_opens_the_preview() {
        use std::os::unix::fs::PermissionsExt;
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        let data = w.t.path().join("data");
        // No group or other bits: the store would reset those to 0700 itself.
        fs::set_permissions(&data, fs::Permissions::from_mode(0o500)).unwrap();
        let job = w.s.begin_scan(&w.id, &w.v).unwrap();
        let r = job.run();
        let summary = w.s.finish_scan(job, r, &w.v);
        fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
        let summary = summary.unwrap();
        assert_eq!(summary.totals.creates, 1);
        assert_eq!(summary.warnings.len(), 1, "{:?}", summary.warnings);
        assert_eq!(summary.warnings[0].code, "io");
    }

    /// A logs folder that can be written but not listed: the log is kept, the
    /// prune error becomes a warning.
    #[cfg(unix)]
    #[test]
    fn a_failed_log_prune_is_a_warning() {
        use std::os::unix::fs::PermissionsExt;
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        let logs = w.s.paths.logs_dir();
        fs::create_dir_all(&logs).unwrap();
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o300)).unwrap();
        let view = w.apply();
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(view.log_path.is_some());
        assert_eq!(view.warnings.len(), 1, "{:?}", view.warnings);
    }

    #[test]
    fn replica_unplugged_after_preview_refuses_apply() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        w.v.unmount("B");
        assert_eq!(
            w.s.begin_apply(&w.v).err().unwrap().code,
            "drive.notConnected"
        );
        assert!(!w.s.is_applying());
        assert!(!w.rep().join("a.txt").exists());
    }

    #[test]
    fn corrupt_store_is_reported_and_never_overwritten() {
        let t = tempfile::tempdir().unwrap();
        let data = t.path().join("data");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("pairs.json"), b"{ damaged").unwrap();
        let err = Session::open(AppPaths::new(data.clone())).err().unwrap();
        assert_eq!(err.code, "store.unreadable");
        assert_eq!(fs::read(data.join("pairs.json")).unwrap(), b"{ damaged");
    }

    #[test]
    fn retry_needs_a_failed_change_and_apply_needs_a_plan() {
        let mut w = world();
        assert_eq!(w.s.begin_apply(&w.v).err().unwrap().code, "apply.noPlan");
        write(&w.src(), "a.txt", b"a");
        w.scan();
        w.apply();
        assert_eq!(
            w.s.begin_retry(&w.v).err().unwrap().code,
            "apply.nothingToRetry"
        );
    }

    #[test]
    fn moving_the_backup_onto_the_source_drive_asks_first() {
        let mut w = world();
        fs::create_dir_all(w.t.path().join("diskA/Backup")).unwrap();
        let mut input = PairInput {
            id: Some(w.id.clone()),
            name: "Photos".into(),
            source: None,
            replica: Some(w.t.path().join("diskA/Backup")),
            user_rules: vec![],
            trash_days: 30,
            allow_same_volume: false,
        };
        assert_eq!(
            w.s.save_pair(input.clone(), &w.v).unwrap_err().code,
            "pair.sameVolume"
        );
        input.allow_same_volume = true;
        assert_eq!(w.s.save_pair(input, &w.v).unwrap().replica.volume_id, "A");
    }

    #[test]
    fn swapping_both_folders_checks_the_new_pair_not_the_old_one() {
        let mut w = world();
        fs::create_dir_all(w.t.path().join("diskB/Photos")).unwrap();
        fs::create_dir_all(w.t.path().join("diskA/Backup")).unwrap();
        let input = PairInput {
            id: Some(w.id.clone()),
            name: "Photos".into(),
            source: Some(w.t.path().join("diskB/Photos")),
            replica: Some(w.t.path().join("diskA/Backup")),
            user_rules: vec![],
            trash_days: 30,
            allow_same_volume: false,
        };
        let p = w.s.save_pair(input, &w.v).unwrap();
        assert_eq!(
            (p.source.volume_id.as_str(), p.replica.volume_id.as_str()),
            ("B", "A")
        );
    }

    #[test]
    fn editing_keeps_history_unless_a_folder_changes() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        w.apply();
        let mut input = PairInput {
            id: Some(w.id.clone()),
            name: "Renamed".into(),
            source: None,
            replica: None,
            user_rules: vec!["*.tmp".into()],
            trash_days: 10,
            allow_same_volume: false,
        };
        let p = w.s.save_pair(input.clone(), &w.v).unwrap();
        assert_eq!(p.name, "Renamed");
        assert!(p.last_sync.is_some());
        fs::create_dir_all(w.t.path().join("diskB/Other")).unwrap();
        input.replica = Some(w.t.path().join("diskB/Other"));
        let p = w.s.save_pair(input, &w.v).unwrap();
        assert!(p.last_sync.is_none(), "a new backup folder is a first sync");
    }

    #[test]
    fn save_pair_validates_name_and_rules() {
        let mut w = world();
        let bad = PairInput {
            id: Some(w.id.clone()),
            name: "  ".into(),
            source: None,
            replica: None,
            user_rules: vec![],
            trash_days: 30,
            allow_same_volume: false,
        };
        assert_eq!(
            w.s.save_pair(bad.clone(), &w.v).err().unwrap().code,
            "pair.nameRequired"
        );
        let bad = PairInput {
            name: "x".into(),
            user_rules: vec!["[".into()],
            ..bad
        };
        assert_eq!(w.s.save_pair(bad, &w.v).err().unwrap().code, "pair.badRule");
    }

    #[test]
    fn a_trash_age_past_the_maximum_is_refused() {
        let mut w = world();
        let input = PairInput {
            id: Some(w.id.clone()),
            name: "Photos".into(),
            source: None,
            replica: None,
            user_rules: vec![],
            trash_days: MAX_TRASH_DAYS + 1,
            allow_same_volume: false,
        };
        assert_eq!(
            w.s.save_pair(input.clone(), &w.v).unwrap_err().code,
            "pair.badTrashDays"
        );
        let ok = PairInput {
            trash_days: MAX_TRASH_DAYS,
            ..input
        };
        assert_eq!(w.s.save_pair(ok, &w.v).unwrap().trash_days, MAX_TRASH_DAYS);
        let bad = Settings {
            default_trash_days: MAX_TRASH_DAYS + 1,
            ..Settings::default()
        };
        assert_eq!(w.s.set_settings(bad).unwrap_err().code, "pair.badTrashDays");
    }

    #[test]
    fn a_trash_age_of_zero_is_refused() {
        let mut w = world();
        let input = PairInput {
            id: Some(w.id.clone()),
            name: "Photos".into(),
            source: None,
            replica: None,
            user_rules: vec![],
            trash_days: 0,
            allow_same_volume: false,
        };
        assert_eq!(
            w.s.save_pair(input.clone(), &w.v).err().unwrap().code,
            "pair.badTrashDays"
        );
        let new = PairInput {
            id: None,
            source: Some(w.src()),
            replica: Some(w.rep()),
            ..input
        };
        assert_eq!(
            w.s.save_pair(new, &w.v).err().unwrap().code,
            "pair.badTrashDays"
        );
        assert_eq!(w.s.pair_views(&w.v)[0].pair.trash_days, 30);
        let bad = Settings {
            default_trash_days: 0,
            ..Settings::default()
        };
        assert_eq!(
            w.s.set_settings(bad).err().unwrap().code,
            "pair.badTrashDays"
        );
        assert_eq!(w.s.settings().default_trash_days, 30);
        assert!(!w.s.paths().settings_file().exists());
    }

    #[test]
    fn old_trash_runs_never_include_the_run_just_made() {
        let mut w = world();
        let input = PairInput {
            id: Some(w.id.clone()),
            name: "Photos".into(),
            source: None,
            replica: None,
            user_rules: vec![],
            trash_days: 1,
            allow_same_volume: false,
        };
        w.s.save_pair(input, &w.v).unwrap();
        fs::create_dir_all(w.rep().join(".sync-trash/2019-01-01_000000")).unwrap();
        write(&w.rep(), "gone.txt", b"g");
        write(&w.src(), "keep.txt", b"k");
        w.scan();
        w.s.confirm_guard().unwrap();
        let mut job = w.s.begin_apply(&w.v).unwrap();
        // A stamp older than the trash age, as a clock change or a long run could give.
        job.stamp = "2020-01-01_000000".into();
        let report = job.run_on(&w.v, &mut |_| {});
        assert_eq!(report.trash_run.as_deref(), Some("2020-01-01_000000"));
        let run = w.s.finish_apply(job, report).unwrap();
        assert_eq!(run.old_trash_runs, vec!["2019-01-01_000000".to_string()]);
    }

    #[test]
    fn the_trash_files_folder_is_found_by_ids_only() {
        let mut w = world();
        write(&w.rep(), "old.txt", b"x");
        w.scan();
        w.s.confirm_guard().unwrap();
        let run = w.apply().report.trash_run.expect("the delete made a run");
        let folder = w.s.trash_files_folder(&w.id, &run, &w.v).unwrap();
        assert!(folder.ends_with(Path::new(".sync-trash").join(&run).join("files")));
        assert!(folder.join("old.txt").is_file());
        assert!(w.s.trash_files_folder(&w.id, "../x", &w.v).is_err());
        assert!(
            w.s.trash_files_folder(&w.id, "2020-01-01_000000", &w.v)
                .is_err()
        );
        assert_eq!(
            w.s.trash_files_folder("nope", &run, &w.v).unwrap_err().code,
            "pair.notFound"
        );
    }

    #[test]
    fn trash_works_while_the_source_is_unplugged() {
        let mut w = world();
        write(&w.rep(), "gone.txt", b"g");
        write(&w.src(), "keep.txt", b"k");
        w.scan();
        w.s.confirm_guard().unwrap();
        w.apply();
        w.v.unmount("A");
        let runs = w.s.trash_runs(&w.id, &w.v).unwrap();
        assert_eq!(runs.len(), 1);
        let n =
            w.s.restore(
                &w.id,
                &runs[0].id,
                &[RelPath::new("gone.txt").unwrap()],
                false,
                &w.v,
            )
            .unwrap();
        assert_eq!(n, 1);
        assert!(w.rep().join("gone.txt").exists());
    }

    #[test]
    fn relinking_drops_the_preview() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        fs::create_dir_all(w.t.path().join("diskB/Other")).unwrap();
        let other = w.t.path().join("diskB/Other");
        w.s.relink(&w.id, SideKind::Replica, &other, false, &w.v)
            .unwrap();
        assert_eq!(w.s.begin_apply(&w.v).err().unwrap().code, "apply.noPlan");
        assert!(!w.rep().join("a.txt").exists());
    }

    #[test]
    fn a_second_scan_is_refused_while_one_runs() {
        let mut w = world();
        let _job = w.s.begin_scan(&w.id, &w.v).unwrap();
        assert_eq!(
            w.s.begin_scan(&w.id, &w.v).err().unwrap().code,
            "apply.busy"
        );
    }

    #[test]
    fn stale_scan_finish_changes_nothing() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        let job = w.s.begin_scan(&w.id, &w.v).unwrap();
        let twin = clone_job(&job);
        let (r, r2) = (job.run(), twin.run());
        w.s.finish_scan(job, r, &w.v).unwrap();
        assert_eq!(
            w.s.finish_scan(twin, r2, &w.v).err().unwrap().code,
            "scan.cancelled"
        );
        assert!(w.s.summary().is_ok());
    }

    fn clone_job(j: &ScanJob) -> ScanJob {
        ScanJob {
            pair_id: j.pair_id.clone(),
            resolved: j.resolved.clone(),
            rules: j.rules.clone(),
            case: j.case,
            counters: j.counters.clone(),
            approx_files: j.approx_files,
        }
    }

    #[test]
    fn nothing_can_toggle_while_scanning() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        let job = w.s.begin_scan(&w.id, &w.v).unwrap();
        assert_eq!(
            w.s.toggle(&RelPath::new("a.txt").unwrap())
                .err()
                .unwrap()
                .code,
            "apply.busy"
        );
        let r = job.run();
        w.s.finish_scan(job, r, &w.v).unwrap();
    }

    #[test]
    fn a_drive_swapped_mid_run_stops_it() {
        for (side, stop) in [
            ("B", StopReason::ReplicaDisconnected),
            ("A", StopReason::SourceDisconnected),
        ] {
            let mut w = world();
            for i in 0..5 {
                write(&w.src(), &format!("f{i}.txt"), b"x");
            }
            w.scan();
            let mut job = w.s.begin_apply(&w.v).unwrap();
            job.drive_check_every = Duration::ZERO;
            let root = if side == "B" { w.rep() } else { w.src() };
            let mut swapped = false;
            let report = job.run_on(&w.v, &mut |_| {
                // Another drive now answers at the same path.
                if !swapped {
                    w.v.mount("other", &root);
                    swapped = true;
                }
            });
            assert_eq!(report.stopped, Some(stop), "{side}");
            assert!(report.applied() < 5);
            let run = w.s.finish_apply(job, report).unwrap();
            assert_eq!(run.report.stopped, Some(stop));
        }
    }

    /// Drives whose lookup fails with an I/O error on the calls in `fail`.
    struct FlakyVolumes<'a> {
        inner: &'a FakeVolumes,
        calls: Cell<usize>,
        fail: Vec<usize>,
    }

    impl Volumes for FlakyVolumes<'_> {
        fn volume_of(&self, path: &Path) -> std::io::Result<volume::VolumeInfo> {
            let n = self.calls.get();
            self.calls.set(n + 1);
            if self.fail.contains(&n) {
                return Err(std::io::Error::other("lookup failed"));
            }
            self.inner.volume_of(path)
        }
        fn find(&self, id: &str) -> std::io::Result<Option<volume::VolumeInfo>> {
            self.inner.find(id)
        }
    }

    #[test]
    fn one_failed_drive_lookup_is_tolerated_two_in_a_row_stop_the_run() {
        // Each check makes two lookups (source, then replica).
        for (fail, stopped) in [
            (vec![1], None),
            (vec![1, 3], Some(StopReason::ReplicaDisconnected)),
            (vec![0, 2], Some(StopReason::SourceDisconnected)),
            (vec![1, 5], None),
        ] {
            let mut w = world();
            for i in 0..5 {
                write(&w.rep(), &format!("old{i}.txt"), b"x");
            }
            write(&w.src(), "keep.txt", b"k");
            w.scan();
            w.s.confirm_guard().unwrap();
            let mut job = w.s.begin_apply(&w.v).unwrap();
            job.drive_check_every = Duration::ZERO;
            let flaky = FlakyVolumes {
                inner: &w.v,
                calls: Cell::new(0),
                fail: fail.clone(),
            };
            let report = job.run_on(&flaky, &mut |_| {});
            assert_eq!(report.stopped, stopped, "{fail:?}");
            w.s.finish_apply(job, report).unwrap();
        }
    }

    #[test]
    fn healthy_drives_let_the_run_finish() {
        let mut w = world();
        for i in 0..5 {
            write(&w.src(), &format!("f{i}.txt"), b"x");
        }
        w.scan();
        let mut job = w.s.begin_apply(&w.v).unwrap();
        job.drive_check_every = Duration::ZERO;
        let report = job.run_on(&w.v, &mut |_| {});
        assert_eq!((report.applied(), report.stopped), (5, None));
        w.s.finish_apply(job, report).unwrap();
    }

    #[test]
    fn a_stopped_run_reports_what_was_not_done() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        write(&w.src(), "b.txt", b"b");
        w.scan();
        let job = w.s.begin_apply(&w.v).unwrap();
        w.s.cancel_apply();
        let report = job.run_on(&w.v, &mut |_| {});
        let run = w.s.finish_apply(job, report).unwrap();
        assert!(run.report.stopped.is_some());
        assert_eq!(run.not_done, 2);
        let last = w.s.pair_views(&w.v)[0].pair.last_sync.clone().unwrap();
        assert!(last.stopped);
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(v["notDone"], 2);
    }

    #[test]
    fn a_finished_run_has_nothing_left_undone() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        let run = w.apply();
        assert_eq!(run.not_done, 0);
        assert!(
            !w.s.pair_views(&w.v)[0]
                .pair
                .last_sync
                .as_ref()
                .unwrap()
                .stopped
        );
    }

    #[test]
    fn a_plan_applies_once() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        w.apply();
        assert_eq!(w.s.begin_apply(&w.v).err().unwrap().code, "apply.noPlan");
    }

    #[test]
    fn empty_selection_is_refused() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        w.scan();
        w.s.select_all(false).unwrap();
        assert_eq!(
            w.s.begin_apply(&w.v).err().unwrap().code,
            "apply.nothingSelected"
        );
    }
}
