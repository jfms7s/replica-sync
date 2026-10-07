//! The app's state between commands: pairs, settings, the current preview and
//! the run in progress. Long jobs are split into begin (under the lock, fast),
//! run (no lock, on a worker thread) and finish (under the lock).

use crate::error::AppError;
use crate::paths::AppPaths;
use crate::settings::Settings;
use crate::tree::{ChildrenPage, PlanTree};
use replica_sync_core::diff::CaseMode;
use replica_sync_core::execute::{Control, ExecContext, Progress, RunReport, execute};
use replica_sync_core::model::{RelPath, SideKind};
use replica_sync_core::pairs::{self, LastSync, NewPair, Pair, PairStore, Resolved};
use replica_sync_core::plan::{
    ChangeId, GuardWarning, Totals, first_sync_guard, selected_totals, space_shortfall,
};
use replica_sync_core::rules::{BUILTIN_PATTERNS, SkipRules};
use replica_sync_core::runlog;
use replica_sync_core::safety::ReplicaRoot;
use replica_sync_core::session::{Prepared, ScanStats, SessionCounters, check_roots, prepare};
use replica_sync_core::trash::{self, OnConflict, TrashRunContents, TrashRunInfo};
use replica_sync_core::volume::{self, Volumes};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

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
    pub delete_folders: Vec<RelPath>,
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
    free_space: u64,
    guard_confirmed: bool,
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
}

impl ApplyJob {
    pub fn run(&self, on_progress: &mut dyn FnMut(&Progress)) -> RunReport {
        let ctx = ExecContext {
            source_root: &self.source_root,
            replica: &self.replica,
            control: &self.control,
            trash_stamp: self.stamp.clone(),
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
        if settings.default_trash_days == 0 {
            return Err(AppError::new("pair.badTrashDays"));
        }
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
        // An age of 0 would offer to empty the trash run the next apply makes.
        if input.trash_days == 0 {
            return Err(AppError::new("pair.badTrashDays"));
        }
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
                if let Some(folder) = &input.source {
                    pairs::relink(&mut pair, SideKind::Source, folder, volumes)?;
                }
                if let Some(folder) = &input.replica {
                    pairs::relink(&mut pair, SideKind::Replica, folder, volumes)?;
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
        volumes: &dyn Volumes,
    ) -> Result<Pair, AppError> {
        self.ensure_idle()?;
        let mut pair = self.pair(id)?.clone();
        pairs::relink(&mut pair, side, folder, volumes)?;
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
        let free_space = volume::free_space(&job.resolved.replica_root)?;
        let pair = self
            .store
            .pairs
            .iter_mut()
            .find(|p| p.id == job.pair_id)
            .ok_or_else(|| AppError::new("pair.notFound").with("id", &job.pair_id))?;
        let first_sync = pair.is_first_sync();
        pair.last_scan_files = Some(prepared.source.files);
        self.store.save()?;
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
            guard_confirmed: false,
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
        let free_space = volume::free_space(&c.resolved.replica_root).unwrap_or(c.free_space);
        let shortfall = space_shortfall(selected.bytes_to_copy, free_space);
        let guard = if c.first_sync {
            first_sync_guard(plan, &c.selection, c.prepared.replica_files)
        } else {
            None
        };
        Ok(PreviewSummary {
            pair_id: c.pair_id.clone(),
            pair_name: pair.name.clone(),
            totals: plan.totals.clone(),
            selected,
            selected_count: c.selection.len(),
            actionable_count: plan.changes.len() - plan.totals.skipped,
            is_empty: plan.is_empty(),
            guard,
            guard_confirmed: c.guard_confirmed,
            shortfall,
            source: c.prepared.source.clone(),
            replica: c.prepared.replica.clone(),
            delete_folders: c.delete_folders.clone(),
        })
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
        self.current_mut()?.guard_confirmed = true;
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
        if now.source_root != c.resolved.source_root || now.replica_root != c.resolved.replica_root
        {
            return Err(AppError::new("drive.wrongVolume").with("path", now.replica_root.display()));
        }
        pairs::verify_source(pair, &c.resolved, volumes)?;
        pairs::verify_replica(pair, &c.resolved, volumes)?;
        Ok(())
    }

    fn start_job(&mut self, approved: HashSet<ChangeId>) -> Result<ApplyJob, AppError> {
        let c = self.current()?;
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
        let _ = runlog::prune(&logs, runlog::LOG_RETENTION, SystemTime::now());
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
        let old_trash_runs: Vec<String> = trash::runs_older_than(
            &replica_root,
            trash_days,
            chrono::Local::now().naive_local(),
        )
        .unwrap_or_default()
        .into_iter()
        .filter(|id| report.trash_run.as_ref() != Some(id))
        .collect();
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
            let report = job.run(&mut |_| {});
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
        assert!(s.guard.is_some() && !s.guard_confirmed);
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
        let report = job.run(&mut |_| {});
        assert_eq!(report.trash_run.as_deref(), Some("2020-01-01_000000"));
        let run = w.s.finish_apply(job, report).unwrap();
        assert_eq!(run.old_trash_runs, vec!["2019-01-01_000000".to_string()]);
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
        w.s.relink(&w.id, SideKind::Replica, &other, &w.v).unwrap();
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
    fn a_stopped_run_reports_what_was_not_done() {
        let mut w = world();
        write(&w.src(), "a.txt", b"a");
        write(&w.src(), "b.txt", b"b");
        w.scan();
        let job = w.s.begin_apply(&w.v).unwrap();
        w.s.cancel_apply();
        let report = job.run(&mut |_| {});
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
