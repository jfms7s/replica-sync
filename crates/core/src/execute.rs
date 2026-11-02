//! Apply the approved changes of a plan to the replica, one at a time, in plan
//! order.
//!
//! Copies go through a [`Batch`]: each is written to a temp name, a whole batch
//! is made durable with one flush, and only then are its temps renamed into place
//! (in plan order). A crash or unplug at any point leaves at most temps, never a
//! partial file under a real name; the next run's scan removes leftover temps.

use crate::model::{Change, MoveKind, RelPath, modified_ns};
use crate::plan::{ChangeId, Plan, Planned};
pub use crate::reason::StopReason;
use crate::reason::{FailReason, SkipReason};
use crate::rules::{CASE_RENAME_SUFFIX, TEMP_SUFFIX};
use crate::safety::{self, ReplicaRoot, SafetyError};
use crate::trash::{TrashReason, TrashWriter};
use filetime::FileTime;
use serde::Serialize;
use std::cell::Cell;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const CHUNK: usize = 8 * 1024 * 1024;
#[cfg(windows)]
const DISK_FULL_CODES: &[i32] = &[39, 112]; // ERROR_HANDLE_DISK_FULL, ERROR_DISK_FULL
#[cfg(not(windows))]
const DISK_FULL_CODES: &[i32] = &[28]; // ENOSPC
#[cfg(windows)]
const IN_USE_CODES: &[i32] = &[32, 33]; // ERROR_SHARING_VIOLATION, ERROR_LOCK_VIOLATION
#[cfg(not(windows))]
const IN_USE_CODES: &[i32] = &[];

const RUN: u8 = 0;
const PAUSE: u8 = 1;
const CANCEL: u8 = 2;

/// Pause / resume / cancel, shared between the UI thread and the executor.
#[derive(Debug, Default)]
pub struct Control {
    state: AtomicU8,
}

impl Control {
    pub fn pause(&self) {
        let _ = self
            .state
            .compare_exchange(RUN, PAUSE, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn resume(&self) {
        let _ = self
            .state
            .compare_exchange(PAUSE, RUN, Ordering::SeqCst, Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.state.store(CANCEL, Ordering::SeqCst);
    }

    pub fn is_paused(&self) -> bool {
        self.state.load(Ordering::SeqCst) == PAUSE
    }

    /// Blocks while paused; false once cancelled.
    fn proceed(&self) -> bool {
        loop {
            match self.state.load(Ordering::SeqCst) {
                RUN => return true,
                CANCEL => return false,
                _ => thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Progress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub changes_done: usize,
    pub changes_total: usize,
    pub current: Option<RelPath>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "reason", rename_all = "camelCase")]
pub enum Outcome {
    Applied,
    Skipped(SkipReason),
    Failed(FailReason),
}

#[derive(Clone, Debug, Serialize)]
pub struct ChangeResult {
    pub id: ChangeId,
    pub path: RelPath,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct RunReport {
    pub results: Vec<ChangeResult>,
    pub stopped: Option<StopReason>,
    pub trash_run: Option<String>,
    pub durability: Durability,
}

/// How a run made its copies durable before renaming them; the run log shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Durability {
    /// True when copies were flushed in batches: one `syncfs` per batch on
    /// Linux, `FlushFileBuffers` on each of the batch's temps on Windows.
    pub batched: bool,
    /// The replica's filesystem when batched, otherwise why not.
    pub why: String,
    /// Batched-run copies on another device than the replica root (a mount or a
    /// btrfs subvolume inside it, which reports its own `st_dev`), each synced
    /// on its own.
    pub synced_separately: usize,
}

impl RunReport {
    fn count(&self, f: impl Fn(&Outcome) -> bool) -> usize {
        self.results.iter().filter(|r| f(&r.outcome)).count()
    }
    pub fn applied(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Applied))
    }
    pub fn skipped(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Skipped(_)))
    }
    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Failed(_)))
    }
    /// For "Retry failed": call [`execute`] again on the SAME [`Plan`] with these
    /// ids approved; it re-checks every change against the disk before applying it.
    /// A [`ChangeId`] is a position in one plan, so these ids mean nothing for any
    /// other plan (a re-plan renumbers its changes).
    pub fn failed_ids(&self) -> HashSet<ChangeId> {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Failed(_)))
            .map(|r| r.id)
            .collect()
    }
}

pub struct ExecContext<'a> {
    pub source_root: &'a Path,
    pub replica: &'a ReplicaRoot,
    pub control: &'a Control,
    pub trash_stamp: String,
    /// Re-checks that the saved drives are still at both roots. `None` keeps
    /// only the "root is a folder" checks (CLI, tests).
    pub drive_check: Option<DriveCheck<'a>>,
}

/// A drive identity check run at the per-change checkpoint: before the first
/// change, before a batch's first rename into place, and otherwise at most
/// once per `every`. An `Err` stops the run as fatal. On Unix each checkpoint
/// also compares both roots' `st_dev` with the run's start (one `stat` each),
/// so a drive unmounted from under a fixed mount point stops the run at the
/// next change, not up to `every` later.
pub struct DriveCheck<'a> {
    pub check: &'a dyn Fn() -> Result<(), StopReason>,
    pub every: Duration,
}

/// How often the app re-checks drive identity during a run.
pub const DRIVE_CHECK_EVERY: Duration = Duration::from_secs(2);

/// Why one change did not apply.
enum Stop {
    Skip(SkipReason),
    Fail(FailReason),
    Fatal(StopReason),
    Cancelled,
}

fn io_stop(e: &io::Error, replica_root: &Path) -> Stop {
    if !replica_root.is_dir() {
        return Stop::Fatal(StopReason::ReplicaDisconnected);
    }
    let code = e.raw_os_error();
    if e.kind() == io::ErrorKind::StorageFull || code.is_some_and(|c| DISK_FULL_CODES.contains(&c))
    {
        return Stop::Fatal(StopReason::ReplicaFull);
    }
    if code.is_some_and(|c| IN_USE_CODES.contains(&c)) {
        return Stop::Fail(FailReason::InUse);
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        return Stop::Fail(FailReason::PermissionDenied);
    }
    Stop::Fail(FailReason::Io {
        detail: e.to_string(),
    })
}

/// A source-side error other than "not found", the same for every change kind:
/// fatal when the source root is gone, otherwise a (retryable) failure.
fn source_stop(e: &io::Error, ctx: &ExecContext) -> Stop {
    if !ctx.source_root.is_dir() {
        return Stop::Fatal(StopReason::SourceDisconnected);
    }
    if e.raw_os_error().is_some_and(|c| IN_USE_CODES.contains(&c)) {
        return Stop::Fail(FailReason::InUse);
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        return Stop::Fail(FailReason::PermissionDenied);
    }
    Stop::Fail(FailReason::Io {
        detail: e.to_string(),
    })
}

/// A source root that vanished is fatal, whatever the change found.
fn source_gone_or(stop: Stop, ctx: &ExecContext) -> Stop {
    if ctx.source_root.is_dir() {
        stop
    } else {
        Stop::Fatal(StopReason::SourceDisconnected)
    }
}

/// The error for a capital-letters-only rename whose roll-back also failed:
/// it says where the file now is.
fn stranded_detail(e: &io::Error, aside: &RelPath) -> String {
    format!("{e}; the file was left at {aside}")
}

/// A replica root that vanished mid-change is fatal, whatever the change found.
fn replica_gone_or(stop: Stop, root: &Path) -> Stop {
    if root.is_dir() {
        stop
    } else {
        Stop::Fatal(StopReason::ReplicaDisconnected)
    }
}

fn safety_stop(e: SafetyError, replica_root: &Path) -> Stop {
    match e {
        SafetyError::Escapes(_) => {
            replica_gone_or(Stop::Fail(FailReason::OutsideReplica), replica_root)
        }
        SafetyError::Io(e) => io_stop(&e, replica_root),
    }
}

/// A [`ReplicaRoot`] action's error as a [`Stop`].
fn act<T>(ctx: &ExecContext, r: Result<T, SafetyError>) -> Result<T, Stop> {
    r.map_err(|e| safety_stop(e, ctx.replica.path()))
}

fn target(ctx: &ExecContext, rel: &RelPath) -> Result<PathBuf, Stop> {
    ctx.replica
        .target(rel)
        .map_err(|e| safety_stop(e, ctx.replica.path()))
}

fn with_suffix(target: &Path, suffix: &str) -> PathBuf {
    let mut name = target
        .file_name()
        .expect("targets have a name")
        .to_os_string();
    name.push(suffix);
    target.with_file_name(name)
}

fn temp_path(target: &Path) -> PathBuf {
    with_suffix(target, TEMP_SUFFIX)
}

/// Intermediate name for a capital-letters-only rename (see `CASE_RENAME_SUFFIX`).
fn case_rename_path(from: &Path) -> PathBuf {
    with_suffix(from, CASE_RENAME_SUFFIX)
}

pub fn execute(
    plan: &Plan,
    approved: &HashSet<ChangeId>,
    ctx: &ExecContext,
    on_progress: &mut dyn FnMut(&Progress),
) -> RunReport {
    let todo: Vec<&Planned> = plan
        .changes
        .iter()
        .filter(|p| approved.contains(&p.id) && !matches!(p.change, Change::Skipped { .. }))
        .collect();
    let mut progress = Progress {
        bytes_total: todo.iter().map(|p| p.change.bytes_to_copy()).sum(),
        changes_total: todo.len(),
        ..Progress::default()
    };
    let mut run = Run {
        ctx,
        disk: durable::Replica::open(ctx.replica.path()),
        trash: TrashWriter::for_replica(ctx.replica, ctx.trash_stamp.clone()),
        report: RunReport::default(),
        batch: Batch::default(),
        progress: &mut progress,
        on_progress,
        last_drive_check: Cell::new(None),
        root_devs: ctx.drive_check.as_ref().and_then(|_| root_devs(ctx)),
    };
    for p in todo {
        let copy = match &p.change {
            Change::Create {
                path,
                size,
                mtime_ns,
            }
            | Change::Update {
                path,
                size,
                mtime_ns,
                ..
            } => Some((path, *size, *mtime_ns)),
            _ => None,
        };
        let full = run.batch.is_full(run.disk.batch_files());
        if (copy.is_none() || full) && !run.flush(Commit::All) {
            break;
        }
        if ctx.control.is_paused() {
            run.disk.park();
        }
        if let Some(stop) = run.check_roots() {
            run.stop(stop);
            break;
        }
        run.progress.current = Some(p.change.path().clone());
        (run.on_progress)(run.progress);
        let result = match copy {
            Some((rel, size, mtime_ns)) => {
                let replaces = matches!(p.change, Change::Update { .. });
                let start = run.progress.bytes_done;
                let r = run.write_copy(p.id, rel, size, mtime_ns, replaces);
                // Count exactly the planned size: a copy that ends up not
                // applied is taken out of the total (see also `flush`).
                run.progress.bytes_done = start + if r.is_ok() { size } else { 0 };
                if r.is_err() {
                    run.progress.bytes_total -= size;
                }
                r
            }
            None => apply_one(&p.change, ctx, &mut run.trash),
        };
        let outcome = match result {
            Ok(()) => Outcome::Applied,
            Err(Stop::Skip(r)) => Outcome::Skipped(r),
            Err(Stop::Fail(r)) => Outcome::Failed(r),
            Err(Stop::Cancelled) => {
                run.stop(StopReason::Cancelled);
                break;
            }
            Err(Stop::Fatal(r)) => {
                // A full disk still holds the batch's whole temps: keep the new
                // files, which need no room in the trash.
                if r != StopReason::ReplicaFull || run.flush(Commit::CreatesOnly) {
                    run.stop(r);
                }
                run.interrupted(p.id, p.change.path());
                break;
            }
        };
        if copy.is_some() && outcome == Outcome::Applied {
            continue; // written to a temp; `flush` reports it
        }
        run.record(p.id, p.change.path(), outcome);
    }
    if run.report.stopped.is_none() {
        run.flush(Commit::All);
    }
    let mut report = run.report;
    report.trash_run = run.trash.run_id().map(str::to_owned);
    report.durability = run.disk.durability();
    report
}

/// Bytes of temps a batch may hold before it is flushed.
const BATCH_BYTES: u64 = 64 * 1024 * 1024;

/// A copy whose temp is complete but not yet durable, so not yet renamed.
struct Written {
    id: ChangeId,
    rel: RelPath,
    dest: PathBuf,
    tmp: PathBuf,
    src_path: PathBuf,
    len: u64,
    mtime_ns: i64,
    /// An `Update`: renaming it in trashes the old version first.
    replaces: bool,
    /// Already synced on its own, so the batch flush need not cover it.
    synced: bool,
}

/// Which waiting copies a flush renames into place.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Commit {
    All,
    /// The disk is full: only new files, since trashing an old version writes
    /// to the trash manifest. The others are reported interrupted.
    CreatesOnly,
}

enum Pending {
    Written(Written),
    /// A copy that skipped or failed while the batch was open; reported in plan order.
    Done(ChangeResult),
}

/// Copies that are written to temps, waiting for one flush before any is renamed
/// into place: nothing reaches a real name before its data is on disk.
#[derive(Default)]
struct Batch {
    items: Vec<Pending>,
    bytes: u64,
}

impl Batch {
    /// `files`: how many copies this run's disk lets wait for one flush.
    fn is_full(&self, files: usize) -> bool {
        self.items.len() >= files || self.bytes >= BATCH_BYTES
    }
}

/// The state of one `execute` call.
struct Run<'a, 'p> {
    ctx: &'a ExecContext<'a>,
    disk: durable::Replica,
    trash: TrashWriter,
    report: RunReport,
    batch: Batch,
    progress: &'p mut Progress,
    on_progress: &'p mut dyn FnMut(&Progress),
    last_drive_check: Cell<Option<Instant>>,
    /// (source, replica) device ids at the start, when a drive check is set.
    root_devs: Option<(u64, u64)>,
}

#[cfg(unix)]
fn root_devs(ctx: &ExecContext) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let src = fs::metadata(ctx.source_root).ok()?.dev();
    let rep = fs::metadata(ctx.replica.path()).ok()?.dev();
    Some((src, rep))
}

#[cfg(not(unix))]
fn root_devs(_: &ExecContext) -> Option<(u64, u64)> {
    None
}

impl Run<'_, '_> {
    fn check_roots(&self) -> Option<StopReason> {
        if !self.ctx.control.proceed() {
            Some(StopReason::Cancelled)
        } else if !self.ctx.replica.path().is_dir() {
            Some(StopReason::ReplicaDisconnected)
        } else if !self.ctx.source_root.is_dir() {
            Some(StopReason::SourceDisconnected)
        } else {
            self.check_drives()
        }
    }

    /// The saved drives are still the ones at both roots (rate-limited).
    fn check_drives(&self) -> Option<StopReason> {
        let dc = self.ctx.drive_check.as_ref()?;
        if let Some((src, rep)) = self.root_devs {
            let now = root_devs(self.ctx);
            if now.is_some_and(|(_, r)| r != rep) {
                return Some(StopReason::ReplicaDisconnected);
            }
            if now.is_some_and(|(s, _)| s != src) {
                return Some(StopReason::SourceDisconnected);
            }
        }
        let now = Instant::now();
        if self
            .last_drive_check
            .get()
            .is_some_and(|t| now.duration_since(t) < dc.every)
        {
            return None;
        }
        self.last_drive_check.set(Some(now));
        (dc.check)().err()
    }

    /// Reports one finished change and counts it as done.
    fn finish(&mut self, id: ChangeId, path: &RelPath, outcome: Outcome) {
        self.push_result(id, path, outcome);
        self.progress.changes_done += 1;
        (self.on_progress)(self.progress);
    }

    /// Reports the change a fatal stop cut short; it never counts as done.
    fn interrupted(&mut self, id: ChangeId, path: &RelPath) {
        self.push_result(id, path, Outcome::Failed(FailReason::Interrupted));
    }

    /// The one place a result enters the report.
    fn push_result(&mut self, id: ChangeId, path: &RelPath, outcome: Outcome) {
        self.report.results.push(ChangeResult {
            id,
            path: path.clone(),
            outcome,
        });
    }

    /// Takes a written copy that will not be applied out of the byte counts.
    fn uncount(&mut self, len: u64) {
        self.progress.bytes_done -= len;
        self.progress.bytes_total -= len;
    }

    /// Reports a change now, or after the open batch so results keep plan order.
    fn record(&mut self, id: ChangeId, path: &RelPath, outcome: Outcome) {
        if self.batch.items.is_empty() {
            self.finish(id, path, outcome);
        } else {
            self.batch.items.push(Pending::Done(ChangeResult {
                id,
                path: path.clone(),
                outcome,
            }));
        }
    }

    /// Ends the run: copies still waiting in the batch are not done, so their
    /// temps are removed and they stay out of the report.
    fn stop(&mut self, reason: StopReason) {
        self.discard();
        self.report.stopped = Some(reason);
    }

    /// Drops the open batch: removes every waiting temp and reports the changes
    /// that already have an outcome.
    fn discard(&mut self) {
        for item in std::mem::take(&mut self.batch.items) {
            match item {
                Pending::Written(w) => {
                    self.disk.remove_temp(self.ctx.replica.path(), &w.tmp);
                    self.uncount(w.len);
                }
                Pending::Done(r) => self.finish(r.id, &r.path, r.outcome),
            }
        }
        self.batch.bytes = 0;
    }

    /// Copies `rel` into a temp that joins the batch. `Ok` means "written,
    /// pending flush"; `flush` reports the outcome.
    fn write_copy(
        &mut self,
        id: ChangeId,
        rel: &RelPath,
        size: u64,
        mtime_ns: i64,
        replaces: bool,
    ) -> Result<(), Stop> {
        let ctx = self.ctx;
        let root = ctx.replica.path();
        let src_path = rel.to_path(ctx.source_root);
        let before = match fs::metadata(&src_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Stop::Skip(SkipReason::DeletedSinceScan));
            }
            Err(e) => return Err(source_stop(&e, ctx)),
        };
        if before.len() != size || modified_ns(&before) != mtime_ns {
            return Err(Stop::Skip(SkipReason::ChangedSincePreview));
        }
        let dest = act(ctx, ctx.replica.create_parent(rel))?;
        let tmp = temp_path(&dest);
        // Something already at the temp name is a leftover the user kept (its
        // planned Delete, which runs before copies, was unticked) or something
        // that is not a regular file. Never remove it outside the trash.
        match fs::symlink_metadata(&tmp) {
            Ok(_) => return Err(Stop::Fail(FailReason::TargetExists)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_stop(&e, root)),
        }
        // Until `create_new` succeeds the temp name is not ours: an error before
        // that point must not remove whatever is there.
        let src = File::open(&src_path).map_err(|e| source_stop(&e, ctx))?;
        let out = match ctx
            .replica
            .create_new(&safety::with_suffix(rel, TEMP_SUFFIX))
        {
            Ok((_, out)) => out,
            Err(SafetyError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(Stop::Fail(FailReason::TargetExists));
            }
            Err(e) => return Err(safety_stop(e, root)),
        };
        let synced = match write_temp(
            ctx,
            &mut self.disk,
            src,
            out,
            &tmp,
            &before,
            self.progress,
            self.on_progress,
        ) {
            Ok(synced) => synced,
            Err(stop) => {
                self.disk.remove_temp(root, &tmp);
                return Err(stop);
            }
        };
        self.batch.bytes += before.len();
        self.batch.items.push(Pending::Written(Written {
            id,
            rel: rel.clone(),
            dest,
            tmp,
            src_path,
            len: before.len(),
            mtime_ns,
            replaces,
            synced,
        }));
        Ok(())
    }

    /// Makes the batch durable with one flush, then renames its temps into place
    /// in plan order, re-checking each source first. False once the run stopped.
    fn flush(&mut self, mut which: Commit) -> bool {
        let root = self.ctx.replica.path();
        if self
            .batch
            .items
            .iter()
            .any(|i| matches!(i, Pending::Written(_)))
        {
            // Re-check the drives before the batch's first rename into place.
            self.last_drive_check.set(None);
        }
        let unsynced = self
            .batch
            .items
            .iter()
            .any(|i| matches!(i, Pending::Written(w) if !w.synced));
        let mut sync_failed = None;
        // A disk that filled up during the flush: temps that did reach the disk
        // still become new files (as for a full disk while writing); the rest
        // are interrupted and the run stops after the batch.
        let mut full = false;
        if unsynced && let Err(e) = self.disk.flush() {
            match io_stop(&e, root) {
                Stop::Fatal(StopReason::ReplicaFull) => {
                    full = true;
                    which = Commit::CreatesOnly;
                }
                Stop::Fatal(r) => {
                    // Copies waiting for this flush were cut short: report them.
                    for item in std::mem::take(&mut self.batch.items) {
                        match item {
                            Pending::Written(w) => {
                                self.disk.remove_temp(root, &w.tmp);
                                self.uncount(w.len);
                                self.interrupted(w.id, &w.rel);
                            }
                            Pending::Done(r) => self.finish(r.id, &r.path, r.outcome),
                        }
                    }
                    self.batch.bytes = 0;
                    self.stop(r);
                    return false;
                }
                Stop::Fail(r) => sync_failed = Some(r),
                Stop::Skip(_) | Stop::Cancelled => unreachable!("io_stop only fails"),
            }
        }
        let flush_failed = full || sync_failed.is_some();
        let mut items = std::mem::take(&mut self.batch.items).into_iter();
        self.batch.bytes = 0;
        while let Some(item) = items.next() {
            let w = match item {
                Pending::Done(r) => {
                    self.finish(r.id, &r.path, r.outcome);
                    continue;
                }
                Pending::Written(w) => w,
            };
            let durable = w.synced || !flush_failed || self.disk.flushed_despite_error(&w.tmp);
            if (full && !durable)
                || which == Commit::CreatesOnly
                    && (w.replaces || fs::symlink_metadata(&w.dest).is_ok())
            {
                self.disk.remove_temp(root, &w.tmp);
                self.uncount(w.len);
                self.interrupted(w.id, &w.rel);
                continue;
            }
            let result = match &sync_failed {
                Some(r) if !durable => Err(Stop::Fail(r.clone())),
                _ => match self.check_roots() {
                    Some(stop) => {
                        self.batch.items =
                            std::iter::once(Pending::Written(w)).chain(items).collect();
                        self.stop(stop);
                        return false;
                    }
                    None => self.commit(&w),
                },
            };
            let outcome = match result {
                Ok(()) => Outcome::Applied,
                Err(Stop::Skip(r)) => Outcome::Skipped(r),
                Err(Stop::Fail(r)) => Outcome::Failed(r),
                Err(Stop::Cancelled) => unreachable!("commit never waits on the control"),
                Err(Stop::Fatal(r)) => {
                    self.disk.remove_temp(root, &w.tmp);
                    self.uncount(w.len);
                    self.interrupted(w.id, &w.rel);
                    self.batch.items = items.collect();
                    self.stop(r);
                    return false;
                }
            };
            if outcome != Outcome::Applied {
                self.disk.remove_temp(root, &w.tmp);
                self.uncount(w.len);
            }
            self.finish(w.id, &w.rel, outcome);
        }
        if full {
            self.stop(StopReason::ReplicaFull);
            return false;
        }
        true
    }

    /// Renames one durable temp into place, unless its source changed since it
    /// was first read.
    fn commit(&mut self, w: &Written) -> Result<(), Stop> {
        let after = match fs::metadata(&w.src_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Stop::Skip(SkipReason::DeletedSinceScan));
            }
            Err(e) => return Err(source_stop(&e, self.ctx)),
        };
        if after.len() != w.len || modified_ns(&after) != w.mtime_ns {
            return Err(Stop::Fail(FailReason::ChangedDuringCopy));
        }
        replace_with_temp(self.ctx, &w.rel, &w.dest, &mut self.trash)
    }
}

fn apply_one(change: &Change, ctx: &ExecContext, trash: &mut TrashWriter) -> Result<(), Stop> {
    match change {
        Change::Create { .. } | Change::Update { .. } => {
            unreachable!("copies go through Run::write_copy")
        }
        Change::MkDir { path } => {
            // Not following links: the scan never plans a link as a folder.
            match fs::symlink_metadata(path.to_path(ctx.source_root)) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => return Err(Stop::Skip(SkipReason::ChangedSincePreview)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    return Err(source_gone_or(
                        Stop::Skip(SkipReason::DeletedSinceScan),
                        ctx,
                    ));
                }
                Err(e) => return Err(source_stop(&e, ctx)),
            }
            act(ctx, ctx.replica.create_dir_all(path))
        }
        Change::Move { from, to, kind } => move_entry(ctx, from, to, kind),
        Change::Delete { path, .. } => delete_file(ctx, path, trash),
        Change::RmDir { path } => remove_dir(ctx, path),
        Change::Skipped { .. } => Ok(()),
    }
}

/// Copies the source into `out`, the temp just created at `tmp`, and sets its
/// mtime. True when the temp was also synced on its own (otherwise the batch
/// flush makes it durable).
#[allow(clippy::too_many_arguments)]
fn write_temp(
    ctx: &ExecContext,
    disk: &mut durable::Replica,
    mut src: File,
    mut out: File,
    tmp: &Path,
    before: &fs::Metadata,
    progress: &mut Progress,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<bool, Stop> {
    let root = ctx.replica.path();
    let on_io = |e: io::Error| io_stop(&e, root);
    let (start, mut written) = (progress.bytes_done, 0u64);
    durable::write_fault(tmp).map_err(on_io)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = src.read(&mut buf).map_err(on_io)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(on_io)?;
        // Never past the planned size (a source growing mid-copy fails at commit).
        written += n as u64;
        progress.bytes_done = start + written.min(before.len());
        on_progress(progress);
        if ctx.control.is_paused() {
            disk.park();
        }
        if !ctx.control.proceed() {
            return Err(Stop::Cancelled);
        }
    }
    let mtime = FileTime::from_system_time(before.modified().map_err(on_io)?);
    filetime::set_file_handle_times(&out, None, Some(mtime)).map_err(on_io)?;
    // After the mtime: a read-only file may refuse new times on some systems.
    // Best effort: a drive that can't hold them (vfat or exFAT on Linux
    // refuse most modes with EPERM) still gets the content.
    let _ = keep_attributes(&out, tmp, before);
    disk.sync_temp(tmp, out).map_err(on_io)
}

/// What a replica keeps besides content and mtime (spec §5.5): the Unix
/// permission bits (`rwx` for owner, group and others; not setuid, setgid or
/// sticky), and on Windows the read-only and hidden attributes. Not ACLs,
/// owners, alternate data streams or creation time. Only where the replica's
/// filesystem can hold them: the caller ignores an error.
#[cfg(unix)]
fn keep_attributes(out: &File, _tmp: &Path, before: &fs::Metadata) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    out.set_permissions(fs::Permissions::from_mode(before.mode() & 0o777))
}

#[cfg(windows)]
fn keep_attributes(out: &File, _tmp: &Path, before: &fs::Metadata) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY};
    let wanted = before.file_attributes() & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_READONLY);
    if wanted == 0 {
        return Ok(());
    }
    win_attrs::set_on_handle(out, out.metadata()?.file_attributes() | wanted)
}

#[cfg(not(any(unix, windows)))]
fn keep_attributes(_: &File, _: &Path, _: &fs::Metadata) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
mod win_attrs {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY, GetFileAttributesW,
        INVALID_FILE_ATTRIBUTES, SetFileAttributesW,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    /// Sets `attrs` through our own open handle (no second open that could
    /// meet a sharing violation). Times are left as they are.
    pub fn set_on_handle(file: &std::fs::File, attrs: u32) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FileBasicInfo, SetFileInformationByHandle,
        };
        let info = FILE_BASIC_INFO {
            CreationTime: 0,
            LastAccessTime: 0,
            LastWriteTime: 0,
            ChangeTime: 0,
            FileAttributes: attrs & !FILE_ATTRIBUTE_NORMAL,
        };
        // SAFETY: the handle is open for writing and `info` outlives the call.
        let ok = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle() as _,
                FileBasicInfo,
                &info as *const FILE_BASIC_INFO as *const _,
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn set(path: &Path, attrs: u32) -> io::Result<()> {
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated and outlives the call.
        if unsafe { SetFileAttributesW(name.as_ptr(), attrs) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Clears read-only so a temp of a read-only source can be removed.
    pub fn clear_read_only(path: &Path) {
        let name = wide(path);
        // SAFETY: `name` is NUL-terminated and outlives both calls.
        unsafe {
            let a = GetFileAttributesW(name.as_ptr());
            if a != INVALID_FILE_ATTRIBUTES && a & FILE_ATTRIBUTE_READONLY != 0 {
                let rest = a & !FILE_ATTRIBUTE_READONLY;
                SetFileAttributesW(
                    name.as_ptr(),
                    if rest == 0 {
                        FILE_ATTRIBUTE_NORMAL
                    } else {
                        rest
                    },
                );
            }
        }
    }
}

/// Durability for the replica disk.
///
/// A batch is flushed with one `syncfs`, but only where that is a real flush:
/// on ext4, btrfs, xfs and f2fs `syncfs` commits the filesystem and flushes the
/// disk's write cache, and from Linux 5.8 it reports write errors. Elsewhere
/// (FUSE such as ntfs-3g or exfat-fuse, kernel vfat/exfat, tmpfs, anything
/// unknown) `syncfs` may not reach the disk, so each temp gets its own
/// `sync_all` and a batch holds one copy, as before batching existed.
#[cfg(target_os = "linux")]
mod durable {
    use super::Durability;
    use std::ffi::{CStr, CString};
    use std::fs::{self, File};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    /// `statfs` magics (Linux `include/uapi/linux/magic.h`) whose `syncfs`
    /// commits and flushes the disk.
    pub(super) const BATCH_FILESYSTEMS: &[(u32, &str)] = &[
        (0xEF53, "ext4"),
        (0x9123_683E, "btrfs"),
        (0x5846_5342, "xfs"),
        (0xF2F5_2010, "f2fs"),
    ];

    /// Magics named in the log when they rule batching out.
    const OTHER_FILESYSTEMS: &[(u32, &str)] = &[
        (0x6573_5546, "fuse"),
        (0x4d44, "vfat"),
        (0x2011_BAB0, "exfat"),
        (0x7366_746E, "ntfs3"),
        (0x5346_544E, "ntfs"),
        (0x0102_1994, "tmpfs"),
    ];

    /// The first kernel whose `syncfs` reports write errors (errseq).
    const MIN_KERNEL: (u32, u32) = (5, 8);

    pub(super) const BATCH_FILES: usize = 256;

    /// Whether a run may flush in batches: `Ok(filesystem)` when it may,
    /// `Err(why not)` when each copy must be synced on its own.
    pub(super) fn decide(
        f_type: Option<u32>,
        kernel: Option<(u32, u32)>,
    ) -> Result<&'static str, String> {
        let f_type = f_type.ok_or("the replica folder could not be opened")?;
        let name = |t: u32| {
            BATCH_FILESYSTEMS
                .iter()
                .chain(OTHER_FILESYSTEMS)
                .find(|(m, _)| *m == t)
                .map(|(_, n)| (*n).to_owned())
                .unwrap_or_else(|| format!("filesystem type {t:#x}"))
        };
        let Some(&(_, fs)) = BATCH_FILESYSTEMS.iter().find(|(m, _)| *m == f_type) else {
            return Err(format!("{}: syncfs may not reach the disk", name(f_type)));
        };
        match kernel {
            Some(k) if k >= MIN_KERNEL => Ok(fs),
            Some((major, minor)) => Err(format!(
                "{fs} on Linux {major}.{minor}: syncfs reports write errors only from 5.8"
            )),
            None => Err(format!("{fs}: unknown kernel version")),
        }
    }

    /// `major.minor` from a `uname` release such as `6.8.0-45-generic`.
    pub(super) fn parse_kernel(release: &str) -> Option<(u32, u32)> {
        let mut parts = release.split(|c: char| !c.is_ascii_digit());
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    }

    fn kernel() -> Option<(u32, u32)> {
        // SAFETY: `utsname` is plain data; `uname` fills it in.
        let mut u: libc::utsname = unsafe { std::mem::zeroed() };
        // SAFETY: `u` is a valid, writable `utsname`.
        if unsafe { libc::uname(&mut u) } != 0 {
            return None;
        }
        // SAFETY: `uname` NUL-terminates `release`.
        let release = unsafe { CStr::from_ptr(u.release.as_ptr()) };
        parse_kernel(release.to_str().ok()?)
    }

    fn f_type(file: &File) -> Option<u32> {
        // SAFETY: `statfs` is plain data; `fstatfs` fills it in.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `file` is an open descriptor and `st` is writable.
        if unsafe { libc::fstatfs(file.as_raw_fd(), &mut st) } != 0 {
            return None;
        }
        // Magics are 32-bit; `f_type`'s width and sign vary by architecture.
        Some(st.f_type as u32)
    }

    /// An open handle on the replica root: `syncfs` needs a descriptor on the
    /// disk, and temps can still be removed through it after the root folder is
    /// moved or renamed.
    pub struct Replica {
        root: Option<File>,
        dev: u64,
        mode: Result<&'static str, String>,
        synced_separately: usize,
    }

    impl Replica {
        pub fn open(root: &Path) -> Replica {
            let file = File::open(root).ok();
            let dev = file.as_ref().and_then(|f| f.metadata().ok());
            let file = file.filter(|_| dev.is_some());
            #[allow(unused_mut)]
            let mut mode = decide(file.as_ref().and_then(f_type), kernel());
            #[cfg(test)]
            if hooks::FORCE_BATCH.get() && file.is_some() {
                mode = Ok("test");
            }
            Replica {
                root: file,
                dev: dev.map_or(0, |m| m.dev()),
                mode,
                synced_separately: 0,
            }
        }

        pub fn durability(&self) -> Durability {
            Durability {
                batched: self.mode.is_ok(),
                why: match &self.mode {
                    Ok(fs) => (*fs).to_owned(),
                    Err(why) => why.clone(),
                },
                synced_separately: self.synced_separately,
            }
        }

        pub fn batch_files(&self) -> usize {
            if self.mode.is_ok() { BATCH_FILES } else { 1 }
        }

        /// Syncs a finished temp now unless the batch's `syncfs` will cover it.
        /// True when it was synced.
        pub fn sync_temp(&mut self, _tmp: &Path, out: File) -> io::Result<bool> {
            if self.mode.is_ok() {
                #[allow(unused_mut)]
                let mut other = out.metadata()?.dev() != self.dev;
                #[cfg(test)]
                {
                    other |= hooks::OTHER_DEVICE.get();
                }
                if !other {
                    return Ok(false);
                }
                // A mount inside the replica: the root's `syncfs` misses it.
                self.synced_separately += 1;
            }
            #[cfg(test)]
            hooks::FILE_SYNCS.set(hooks::FILE_SYNCS.get() + 1);
            out.sync_all()?;
            Ok(true)
        }

        /// After a failed `syncfs`, no temp of the batch is known to be on disk.
        pub fn flushed_despite_error(&self, _tmp: &Path) -> bool {
            false
        }

        /// Nothing to do before a pause: no temp holds an open handle.
        pub fn park(&mut self) {}

        /// One `syncfs` on the replica root's filesystem.
        pub fn flush(&mut self) -> io::Result<()> {
            let file = self.root.as_ref().expect("batching needs the root open");
            #[cfg(test)]
            {
                hooks::FLUSHES.set(hooks::FLUSHES.get() + 1);
                if let Some((_, errno)) = hooks::FLUSH_ERRNO.get() {
                    return Err(io::Error::from_raw_os_error(errno));
                }
            }
            // SAFETY: `file` is an open descriptor for the duration of the call.
            if unsafe { libc::syncfs(file.as_raw_fd()) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        /// Best effort: a temp that cannot be removed is a leftover the next
        /// preview plans for the trash.
        pub fn remove_temp(&mut self, root: &Path, tmp: &Path) {
            let (Some(dir), Ok(rel)) = (&self.root, tmp.strip_prefix(root)) else {
                let _ = fs::remove_file(tmp);
                return;
            };
            let Ok(rel) = CString::new(rel.as_os_str().as_bytes()) else {
                return;
            };
            // SAFETY: `dir` is an open descriptor and `rel` is NUL-terminated.
            unsafe { libc::unlinkat(dir.as_raw_fd(), rel.as_ptr(), 0) };
        }
    }

    /// Test-only write failure (see `hooks::WRITE_ERRNO`).
    pub fn write_fault(_tmp: &Path) -> io::Result<()> {
        #[cfg(test)]
        if let Some((prefix, errno)) = hooks::WRITE_ERRNO.get()
            && _tmp
                .file_name()
                .is_some_and(|n| n.as_bytes().starts_with(prefix.as_bytes()))
        {
            return Err(io::Error::from_raw_os_error(errno));
        }
        Ok(())
    }

    #[cfg(test)]
    pub mod hooks {
        use std::cell::Cell;
        thread_local! {
            /// Batch flushes run by this thread.
            pub static FLUSHES: Cell<u64> = const { Cell::new(0) };
            /// Temps this thread synced one by one.
            pub static FILE_SYNCS: Cell<u64> = const { Cell::new(0) };
            /// Batch whatever the filesystem (test temp folders are on tmpfs).
            pub static FORCE_BATCH: Cell<bool> = const { Cell::new(false) };
            /// Treat every temp as on another filesystem than the replica root.
            pub static OTHER_DEVICE: Cell<bool> = const { Cell::new(false) };
            /// `syncfs` fails with this errno.
            /// (The name prefix is ignored: one `syncfs` covers every temp.)
            pub static FLUSH_ERRNO: Cell<Option<(&'static str, i32)>> = const { Cell::new(None) };
            /// Writing a temp whose name starts with this fails with the errno.
            pub static WRITE_ERRNO: Cell<Option<(&'static str, i32)>> = const { Cell::new(None) };
        }
    }
}

/// Durability for the replica disk on Windows.
///
/// A volume-wide flush needs admin rights, so a batch is made durable with one
/// `FlushFileBuffers` per temp, run together from a few threads once the whole
/// batch is written. Each temp's handle stays open (it was opened for writing,
/// which `FlushFileBuffers` needs) until that flush, and is closed before any
/// rename. That is a real flush on NTFS, exFAT and FAT32 alike. The batch caps
/// (256 files, 64 MB) bound the open handles.
///
/// Temps are removed by path: a replica root that was moved or unplugged keeps
/// its leftover temps, which the next scan removes.
#[cfg(windows)]
mod durable {
    use super::Durability;
    use std::collections::HashSet;
    use std::fs::{self, File};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::thread;

    pub(super) const BATCH_FILES: usize = 256;

    /// Threads that flush a batch's handles; flushes on one disk overlap in the
    /// OS and the device, so a few in flight beat one at a time.
    const FLUSH_THREADS: usize = 4;

    pub struct Replica {
        /// Finished temps not yet flushed, with their still-open write handles.
        open: Vec<(PathBuf, File)>,
        /// Temps whose flush failed in the last batch flush.
        failed: HashSet<PathBuf>,
        /// Flushed early by `park`: failures and the first error, handed to
        /// the next batch flush.
        parked_failed: HashSet<PathBuf>,
        parked_error: Option<io::Error>,
    }

    impl Replica {
        pub fn open(_root: &Path) -> Replica {
            Replica {
                open: Vec::new(),
                failed: HashSet::new(),
                parked_failed: HashSet::new(),
                parked_error: None,
            }
        }

        pub fn durability(&self) -> Durability {
            Durability {
                batched: true,
                why: "Windows: FlushFileBuffers on each of its temps".into(),
                synced_separately: 0,
            }
        }

        pub fn batch_files(&self) -> usize {
            BATCH_FILES
        }

        /// Keeps the temp's handle for the batch flush; never synced on its own.
        pub fn sync_temp(&mut self, tmp: &Path, out: File) -> io::Result<bool> {
            self.open.push((tmp.to_path_buf(), out));
            Ok(false)
        }

        /// Flushes every waiting temp, then closes their handles (also on
        /// failure). Returns the first error; the temps whose own flush
        /// failed are then the ones `flushed_despite_error` refuses.
        pub fn flush(&mut self) -> io::Result<()> {
            #[cfg(test)]
            hooks::FLUSHES.set(hooks::FLUSHES.get() + 1);
            self.flush_open();
            self.failed = std::mem::take(&mut self.parked_failed);
            self.parked_error.take().map_or(Ok(()), Err)
        }

        /// Flushes and closes the waiting temps now, before a pause, so no
        /// file on the drive stays open while the user may unplug it. Their
        /// outcome is reported by the next `flush`.
        pub fn park(&mut self) {
            #[cfg(test)]
            if !self.open.is_empty() {
                hooks::PARKS.set(hooks::PARKS.get() + 1);
            }
            self.flush_open();
        }

        /// Flushes and closes every open temp, keeping failures for `flush`.
        fn flush_open(&mut self) {
            if self.open.is_empty() {
                return;
            }
            let open = std::mem::take(&mut self.open);
            // Read here: the hooks are per thread and the flush runs on others.
            #[cfg(test)]
            let fault = hooks::FLUSH_ERRNO.get();
            #[cfg(not(test))]
            let fault = None;
            let errors = flush_all(&open, fault);
            self.parked_failed
                .extend(errors.iter().map(|(i, _)| open[*i].0.clone()));
            if self.parked_error.is_none() {
                self.parked_error = errors.into_iter().next().map(|(_, e)| e);
            }
        }

        /// Whether `tmp` reached the disk although the batch flush failed.
        pub fn flushed_despite_error(&self, tmp: &Path) -> bool {
            !self.failed.contains(tmp)
        }

        /// Best effort: a temp that cannot be removed is a leftover the next
        /// preview plans for the trash. Its handle, if still open, is closed first.
        pub fn remove_temp(&mut self, _root: &Path, tmp: &Path) {
            self.open.retain(|(p, _)| p != tmp);
            // A copy of a read-only source is read-only already.
            super::win_attrs::clear_read_only(tmp);
            let _ = fs::remove_file(tmp);
        }
    }

    /// `FlushFileBuffers` on each handle, spread over up to `FLUSH_THREADS`
    /// (inline when a thread can't be started). Every handle is flushed even
    /// after a failure; returns the index and error of each that failed.
    ///
    /// `fault` (tests only): temps whose name starts with it fail with the code.
    fn flush_all(open: &[(PathBuf, File)], fault: Fault) -> Vec<(usize, io::Error)> {
        if open.len() <= 1 {
            return flush_chunk(0, open, fault);
        }
        let per_thread = open.len().div_ceil(FLUSH_THREADS);
        thread::scope(|s| {
            let mut errors = Vec::new();
            let mut workers = Vec::new();
            for (n, chunk) in open.chunks(per_thread).enumerate() {
                let first = n * per_thread;
                let work = move || flush_chunk(first, chunk, fault);
                match thread::Builder::new().spawn_scoped(s, work) {
                    Ok(handle) => workers.push((first, chunk.len(), handle)),
                    Err(_) => errors.extend(flush_chunk(first, chunk, fault)),
                }
            }
            for (first, len, handle) in workers {
                match handle.join() {
                    Ok(e) => errors.extend(e),
                    Err(_) => errors.extend(
                        (first..first + len)
                            .map(|i| (i, io::Error::other("a flush thread panicked"))),
                    ),
                }
            }
            errors.sort_by_key(|(i, _)| *i);
            errors
        })
    }

    /// A test-injected flush failure: temp name prefix and OS error code.
    type Fault = Option<(&'static str, i32)>;

    fn flush_chunk(
        first: usize,
        chunk: &[(PathBuf, File)],
        fault: Fault,
    ) -> Vec<(usize, io::Error)> {
        chunk
            .iter()
            .enumerate()
            .filter_map(|(i, (tmp, f))| {
                if let Some((prefix, code)) = fault
                    && name_starts_with(tmp, prefix)
                {
                    return Some((first + i, io::Error::from_raw_os_error(code)));
                }
                f.sync_all().err().map(|e| (first + i, e))
            })
            .collect()
    }

    fn name_starts_with(tmp: &Path, prefix: &str) -> bool {
        tmp.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(prefix))
    }

    /// Test-only write failure (see `hooks::WRITE_ERRNO`).
    pub fn write_fault(_tmp: &Path) -> io::Result<()> {
        #[cfg(test)]
        if let Some((prefix, code)) = hooks::WRITE_ERRNO.get()
            && name_starts_with(_tmp, prefix)
        {
            return Err(io::Error::from_raw_os_error(code));
        }
        Ok(())
    }

    #[cfg(test)]
    pub mod hooks {
        use std::cell::Cell;
        thread_local! {
            /// Batch flushes run by this thread.
            pub static FLUSHES: Cell<u64> = const { Cell::new(0) };
            /// Early flushes before a pause (`park`) that had temps to flush.
            pub static PARKS: Cell<u64> = const { Cell::new(0) };
            /// Batching is always on here; kept so the tests read the same.
            pub static FORCE_BATCH: Cell<bool> = const { Cell::new(false) };
            /// Flushing a temp whose name starts with this fails with the code
            /// (`""` for every temp).
            pub static FLUSH_ERRNO: Cell<Option<(&'static str, i32)>> = const { Cell::new(None) };
            /// Writing a temp whose name starts with this fails with the code.
            pub static WRITE_ERRNO: Cell<Option<(&'static str, i32)>> = const { Cell::new(None) };
        }
    }
}

/// Durability elsewhere (not built for release): each temp is synced as it is
/// written and a batch holds one copy.
#[cfg(not(any(target_os = "linux", windows)))]
mod durable {
    use super::Durability;
    use std::fs::{self, File};
    use std::io;
    use std::path::Path;

    pub struct Replica;

    impl Replica {
        pub fn open(_root: &Path) -> Replica {
            Replica
        }

        pub fn durability(&self) -> Durability {
            Durability {
                batched: false,
                why: "this platform syncs each file".into(),
                synced_separately: 0,
            }
        }

        pub fn batch_files(&self) -> usize {
            1
        }

        pub fn sync_temp(&mut self, _tmp: &Path, out: File) -> io::Result<bool> {
            out.sync_all()?;
            Ok(true)
        }

        pub fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }

        pub fn flushed_despite_error(&self, _tmp: &Path) -> bool {
            false
        }

        pub fn park(&mut self) {}

        pub fn remove_temp(&mut self, _root: &Path, tmp: &Path) {
            let _ = fs::remove_file(tmp);
        }
    }

    pub fn write_fault(_tmp: &Path) -> io::Result<()> {
        Ok(())
    }
}

fn replace_with_temp(
    ctx: &ExecContext,
    rel: &RelPath,
    dest: &Path,
    trash: &mut TrashWriter,
) -> Result<(), Stop> {
    let root = ctx.replica.path();
    match fs::symlink_metadata(dest) {
        Ok(old) => trash
            .move_in(rel, old.len(), modified_ns(&old), TrashReason::Replaced)
            .map_err(|e| io_stop(&e, root))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_stop(&e, root)),
    }
    act(
        ctx,
        ctx.replica
            .rename(&safety::with_suffix(rel, TEMP_SUFFIX), rel),
    )
}

fn move_entry(
    ctx: &ExecContext,
    from: &RelPath,
    to: &RelPath,
    kind: &MoveKind,
) -> Result<(), Stop> {
    let root = ctx.replica.path();
    let still_planned = match (kind, fs::metadata(to.to_path(ctx.source_root))) {
        (MoveKind::File { size, mtime_ns }, Ok(m)) => {
            m.is_file() && m.len() == *size && modified_ns(&m) == *mtime_ns
        }
        (MoveKind::Dir { .. }, Ok(m)) => m.is_dir(),
        (_, Err(e)) if e.kind() == io::ErrorKind::NotFound => false,
        (_, Err(e)) => return Err(source_stop(&e, ctx)),
    };
    if !still_planned {
        return Err(Stop::Skip(SkipReason::ChangedSincePreview));
    }
    let from_path = target(ctx, from)?;
    let to_path = target(ctx, to)?;
    if fs::symlink_metadata(&from_path).is_err() {
        return Err(replica_gone_or(
            Stop::Skip(SkipReason::NoLongerInReplica),
            root,
        ));
    }
    let case_only = from.fold() == to.fold();
    if !case_only && fs::symlink_metadata(&to_path).is_ok() {
        return Err(Stop::Fail(FailReason::TargetExists));
    }
    act(ctx, ctx.replica.create_parent(to))?;
    if case_only {
        if fs::symlink_metadata(case_rename_path(&from_path)).is_ok() {
            return Err(Stop::Fail(FailReason::TargetExists));
        }
        let aside = safety::with_suffix(from, CASE_RENAME_SUFFIX);
        act(ctx, ctx.replica.rename(from, &aside))?;
        match ctx.replica.rename(&aside, to) {
            Ok(()) => Ok(()),
            Err(e) => match ctx.replica.rename(&aside, from) {
                Ok(()) => Err(safety_stop(e, root)),
                Err(_) => {
                    let e = match e {
                        SafetyError::Io(e) => e,
                        e => io::Error::other(e.to_string()),
                    };
                    Err(replica_gone_or(
                        Stop::Fail(FailReason::Io {
                            detail: stranded_detail(&e, &aside),
                        }),
                        root,
                    ))
                }
            },
        }
    } else {
        act(ctx, ctx.replica.rename(from, to))
    }
}

fn delete_file(ctx: &ExecContext, rel: &RelPath, trash: &mut TrashWriter) -> Result<(), Stop> {
    let root = ctx.replica.path();
    match fs::symlink_metadata(rel.to_path(ctx.source_root)) {
        Ok(_) => return Err(Stop::Skip(SkipReason::BackOnSource)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(source_stop(&e, ctx)),
    }
    let dest = target(ctx, rel)?;
    let md = match fs::symlink_metadata(&dest) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(replica_gone_or(Stop::Skip(SkipReason::AlreadyGone), root));
        }
        Err(e) => return Err(io_stop(&e, root)),
    };
    trash
        .move_in(rel, md.len(), modified_ns(&md), TrashReason::Deleted)
        .map_err(|e| io_stop(&e, root))
}

fn remove_dir(ctx: &ExecContext, rel: &RelPath) -> Result<(), Stop> {
    // By exact name: on a source that ignores capital letters, `foo` would
    // otherwise "exist" while only `Foo` does.
    // The listing only when the name resolves at all, which is rare.
    match fs::symlink_metadata(rel.to_path(ctx.source_root)) {
        Ok(_) => {
            let parent = rel
                .parent()
                .expect("never the root")
                .to_path(ctx.source_root);
            for e in fs::read_dir(&parent).map_err(|e| source_stop(&e, ctx))? {
                let e = e.map_err(|e| source_stop(&e, ctx))?;
                if e.file_name().to_str() == Some(rel.name()) {
                    return Err(Stop::Skip(SkipReason::BackOnSource));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if !ctx.source_root.is_dir() {
                return Err(Stop::Fatal(StopReason::SourceDisconnected));
            }
        }
        Err(e) => return Err(source_stop(&e, ctx)),
    }
    match ctx.replica.remove_dir(rel) {
        Ok(()) => Ok(()),
        Err(SafetyError::Io(e)) if e.kind() == io::ErrorKind::NotFound => Err(replica_gone_or(
            Stop::Skip(SkipReason::AlreadyGone),
            ctx.replica.path(),
        )),
        Err(SafetyError::Io(e)) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            Err(Stop::Skip(SkipReason::FolderNotEmpty))
        }
        Err(e) => Err(safety_stop(e, ctx.replica.path())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{T0, ns, plan_for, read_tree, write_file};
    use std::fs;

    pub(super) struct Fixture {
        pub _dir: tempfile::TempDir,
        pub src: PathBuf,
        pub rep: PathBuf,
    }

    pub(super) fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (src, rep) = (dir.path().join("src"), dir.path().join("rep"));
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&rep).unwrap();
        Fixture {
            _dir: dir,
            src,
            rep,
        }
    }

    pub(super) fn run_with(
        f: &Fixture,
        plan: &Plan,
        approved: &HashSet<ChangeId>,
        control: &Control,
        on_progress: &mut dyn FnMut(&Progress),
    ) -> RunReport {
        let replica = ReplicaRoot::new(&f.rep).unwrap();
        let ctx = ExecContext {
            source_root: &f.src,
            replica: &replica,
            control,
            trash_stamp: "2026-10-07_120000".into(),
            drive_check: None,
        };
        execute(plan, approved, &ctx, on_progress)
    }

    pub(super) fn run_all(f: &Fixture) -> RunReport {
        let plan = plan_for(&f.src, &f.rep);
        run_with(
            f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        )
    }

    pub(super) fn no_temp_files(root: &Path) -> bool {
        read_tree(root).keys().all(|k| !k.ends_with(TEMP_SUFFIX))
    }

    fn run_checking_drives(
        f: &Fixture,
        check: &dyn Fn() -> Result<(), StopReason>,
        every: Duration,
    ) -> RunReport {
        let plan = plan_for(&f.src, &f.rep);
        let replica = ReplicaRoot::new(&f.rep).unwrap();
        let control = Control::default();
        let ctx = ExecContext {
            source_root: &f.src,
            replica: &replica,
            control: &control,
            trash_stamp: "2026-10-07_120000".into(),
            drive_check: Some(DriveCheck { check, every }),
        };
        execute(&plan, &plan.actionable_ids(), &ctx, &mut |_| {})
    }

    #[test]
    fn a_drive_that_stops_matching_mid_run_stops_it() {
        let f = fixture();
        for i in 0..5 {
            write_file(&f.src, &format!("f{i}.txt"), b"x", T0);
        }
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 3 {
                Err(StopReason::ReplicaDisconnected)
            } else {
                Ok(())
            }
        };
        let report = run_checking_drives(&f, &check, Duration::ZERO);
        assert_eq!(report.stopped, Some(StopReason::ReplicaDisconnected));
        assert!(report.applied() < 5, "{report:?}");
        assert!(no_temp_files(&f.rep));
        let copied = (0..5)
            .filter(|i| f.rep.join(format!("f{i}.txt")).exists())
            .count();
        assert_eq!(copied, report.applied());
    }

    #[test]
    fn a_drive_swapped_before_a_batch_is_renamed_keeps_the_batch_out() {
        let f = fixture();
        for i in 0..5 {
            write_file(&f.src, &format!("f{i}.txt"), b"x", T0);
        }
        let calls = Cell::new(0);
        // Fine at the start, gone when the batch is about to be renamed in;
        // the long interval would hide it without the pre-rename check.
        let check = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 2 {
                Err(StopReason::ReplicaDisconnected)
            } else {
                Ok(())
            }
        };
        let report = run_checking_drives(&f, &check, Duration::from_secs(3600));
        assert_eq!(report.stopped, Some(StopReason::ReplicaDisconnected));
        assert_eq!(report.applied(), 0);
        assert!((0..5).all(|i| !f.rep.join(format!("f{i}.txt")).exists()));
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn the_drive_check_runs_first_and_is_rate_limited() {
        let f = fixture();
        for i in 0..5 {
            write_file(&f.src, &format!("f{i}.txt"), b"x", T0);
        }
        let calls = Cell::new(0);
        let check = || {
            calls.set(calls.get() + 1);
            Ok(())
        };
        let report = run_checking_drives(&f, &check, Duration::from_secs(3600));
        assert_eq!((report.applied(), report.stopped), (5, None));
        // Before the first change, then before each batch's renames (one
        // batch, or one per file where the drive can't batch its flushes).
        assert!((2..=6).contains(&calls.get()), "{}", calls.get());

        // Deletes don't batch: only the first change checks within the interval.
        let f = fixture();
        for i in 0..5 {
            write_file(&f.rep, &format!("old{i}.txt"), b"x", T0);
        }
        calls.set(0);
        let report = run_checking_drives(&f, &check, Duration::from_secs(3600));
        assert_eq!((report.applied(), report.stopped), (5, None));
        assert_eq!(calls.get(), 1);

        let f = fixture();
        write_file(&f.src, "a.txt", b"x", T0);
        let gone = || Err(StopReason::SourceDisconnected);
        let report = run_checking_drives(&f, &gone, Duration::from_secs(3600));
        assert_eq!(report.stopped, Some(StopReason::SourceDisconnected));
        assert_eq!(report.applied(), 0);
        assert!(!f.rep.join("a.txt").exists());
    }

    #[test]
    fn create_copies_content_and_mtime_into_new_folders() {
        let f = fixture();
        write_file(&f.src, "a/b/c.txt", b"hello", T0);
        let report = run_all(&f);
        assert_eq!((report.applied(), report.failed()), (1, 0));
        assert_eq!(fs::read(f.rep.join("a/b/c.txt")).unwrap(), b"hello");
        let md = fs::metadata(f.rep.join("a/b/c.txt")).unwrap();
        assert_eq!(modified_ns(&md), ns(T0));
        assert!(no_temp_files(&f.rep));
        assert!(report.trash_run.is_none());
    }

    #[test]
    fn update_trashes_the_old_version() {
        let f = fixture();
        write_file(&f.src, "x.txt", b"new!", T0 + 60);
        write_file(&f.rep, "x.txt", b"old", T0);
        let report = run_all(&f);
        assert_eq!(report.applied(), 1);
        assert_eq!(fs::read(f.rep.join("x.txt")).unwrap(), b"new!");
        let run = report.trash_run.unwrap();
        let c = crate::trash::run_contents(&f.rep, &run).unwrap();
        assert_eq!(c.items[0].reason, crate::trash::TrashReason::Replaced);
    }

    #[test]
    fn mkdir_creates_empty_folder() {
        let f = fixture();
        fs::create_dir_all(f.src.join("empty")).unwrap();
        assert_eq!(run_all(&f).applied(), 1);
        assert!(f.rep.join("empty").is_dir());
    }

    #[test]
    fn source_deleted_or_changed_after_preview_is_skipped() {
        let f = fixture();
        write_file(&f.src, "gone.txt", b"1", T0);
        write_file(&f.src, "edited.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        fs::remove_file(f.src.join("gone.txt")).unwrap();
        write_file(&f.src, "edited.txt", b"22", T0 + 9);
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        let reasons: Vec<_> = report.results.iter().map(|r| r.outcome.clone()).collect();
        assert!(reasons.contains(&Outcome::Skipped(SkipReason::DeletedSinceScan)));
        assert!(reasons.contains(&Outcome::Skipped(SkipReason::ChangedSincePreview)));
        assert!(read_tree(&f.rep).is_empty());
    }

    #[test]
    fn source_growing_during_copy_fails_and_cleans_up() {
        let f = fixture();
        write_file(&f.src, "grow.log", b"start", T0);
        let plan = plan_for(&f.src, &f.rep);
        let grow = f.src.join("grow.log");
        let mut appended = false;
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                if !appended
                    && p.bytes_done > 0
                    && p.current.as_ref().is_some_and(|c| c.as_str() == "grow.log")
                {
                    use std::io::Write;
                    fs::OpenOptions::new()
                        .append(true)
                        .open(&grow)
                        .unwrap()
                        .write_all(b" more")
                        .unwrap();
                    appended = true;
                }
            },
        );
        let grow_result = report
            .results
            .iter()
            .find(|r| r.path.as_str() == "grow.log")
            .unwrap();
        assert_eq!(
            grow_result.outcome,
            Outcome::Failed(FailReason::ChangedDuringCopy)
        );
        assert!(!f.rep.join("grow.log").exists());
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn unapproved_changes_are_left_alone() {
        let f = fixture();
        write_file(&f.src, "a.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        let report = run_with(&f, &plan, &HashSet::new(), &Control::default(), &mut |_| {});
        assert!(report.results.is_empty());
        assert!(read_tree(&f.rep).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn write_through_symlinked_folder_is_refused() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), f.rep.join("Docs")).unwrap();
        write_file(&f.src, "Docs/x.txt", b"1", T0);
        let plan = crate::plan::build_plan(
            vec![Change::Create {
                path: crate::testutil::rel("Docs/x.txt"),
                size: 1,
                mtime_ns: ns(T0),
            }],
            crate::diff::CaseMode::Sensitive,
        )
        .unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert!(matches!(
            &report.results[0].outcome,
            Outcome::Failed(FailReason::OutsideReplica)
        ));
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn write_through_junctioned_folder_is_refused() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        crate::testutil::junction(&f.rep.join("Docs"), outside.path());
        write_file(&f.src, "Docs/x.txt", b"1", T0);
        let plan = crate::plan::build_plan(
            vec![Change::Create {
                path: crate::testutil::rel("Docs/x.txt"),
                size: 1,
                mtime_ns: ns(T0),
            }],
            crate::diff::CaseMode::Insensitive,
        )
        .unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert!(matches!(
            &report.results[0].outcome,
            Outcome::Failed(FailReason::OutsideReplica)
        ));
        assert!(fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn stale_temp_symlink_is_neither_followed_nor_removed() {
        let f = fixture();
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.txt");
        fs::write(&victim, "outside").unwrap();
        std::os::unix::fs::symlink(&victim, f.rep.join(format!("a.txt{TEMP_SUFFIX}"))).unwrap();
        write_file(&f.src, "a.txt", b"inside", T0);
        let plan = plan_for(&f.src, &f.rep);
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(report.applied(), 0);
        assert!(matches!(
            report.results[0].outcome,
            Outcome::Failed(FailReason::TargetExists)
        ));
        assert_eq!(fs::read(&victim).unwrap(), b"outside");
        let tmp = f.rep.join(format!("a.txt{TEMP_SUFFIX}"));
        assert!(fs::symlink_metadata(&tmp).unwrap().is_symlink());
    }
}

#[cfg(test)]
mod apply_tests {
    use super::tests::{fixture, no_temp_files, run_all, run_with};
    use super::*;
    use crate::testutil::{T0, ns, plan_for, read_tree, rel, write_file};
    use std::fs;

    #[cfg(unix)]
    #[test]
    fn copies_keep_the_unix_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        for (name, m) in [("run.sh", 0o755), ("secret", 0o600), ("ro.txt", 0o444)] {
            write_file(&f.src, name, b"x", T0);
            fs::set_permissions(f.src.join(name), fs::Permissions::from_mode(m)).unwrap();
        }
        // setuid is not copied.
        write_file(&f.src, "suid", b"x", T0);
        fs::set_permissions(f.src.join("suid"), fs::Permissions::from_mode(0o4755)).unwrap();
        let report = run_all(&f);
        assert_eq!((report.applied(), report.failed()), (4, 0));
        assert_eq!(mode(&f.rep.join("run.sh")), 0o755);
        assert_eq!(mode(&f.rep.join("secret")), 0o600);
        assert_eq!(mode(&f.rep.join("ro.txt")), 0o444);
        assert_eq!(mode(&f.rep.join("suid")), 0o755);
        // A read-only replica file is replaced (the old one trashed) on update.
        fs::set_permissions(f.src.join("ro.txt"), fs::Permissions::from_mode(0o644)).unwrap();
        write_file(&f.src, "ro.txt", b"new", T0 + 60);
        fs::set_permissions(f.src.join("ro.txt"), fs::Permissions::from_mode(0o444)).unwrap();
        let report = run_all(&f);
        assert_eq!((report.applied(), report.failed()), (1, 0));
        assert_eq!(fs::read(f.rep.join("ro.txt")).unwrap(), b"new");
        assert_eq!(mode(&f.rep.join("ro.txt")), 0o444);
        assert!(no_temp_files(&f.rep));
    }

    #[cfg(windows)]
    #[test]
    fn copies_keep_read_only_and_hidden_and_update_over_them() {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY,
        };
        let attrs = |p: &Path| fs::metadata(p).unwrap().file_attributes();
        let f = fixture();
        write_file(&f.src, "ro.txt", b"x", T0);
        write_file(&f.src, "hidden.txt", b"x", T0);
        write_file(&f.src, "plain.txt", b"x", T0);
        let mut ro = fs::metadata(f.src.join("ro.txt")).unwrap().permissions();
        ro.set_readonly(true);
        fs::set_permissions(f.src.join("ro.txt"), ro).unwrap();
        let hidden = f.src.join("hidden.txt");
        win_attrs::set(&hidden, attrs(&hidden) | FILE_ATTRIBUTE_HIDDEN).unwrap();
        let report = run_all(&f);
        assert_eq!((report.applied(), report.failed()), (3, 0), "{report:?}");
        assert_ne!(attrs(&f.rep.join("ro.txt")) & FILE_ATTRIBUTE_READONLY, 0);
        assert_ne!(attrs(&f.rep.join("hidden.txt")) & FILE_ATTRIBUTE_HIDDEN, 0);
        assert_eq!(
            attrs(&f.rep.join("plain.txt")) & (FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_HIDDEN),
            0
        );
        // Updating a read-only replica file: the old one goes to the trash.
        win_attrs::clear_read_only(&f.src.join("ro.txt"));
        write_file(&f.src, "ro.txt", b"new", T0 + 60);
        let report = run_all(&f);
        assert_eq!((report.applied(), report.failed()), (1, 0), "{report:?}");
        assert_eq!(fs::read(f.rep.join("ro.txt")).unwrap(), b"new");
        assert_eq!(attrs(&f.rep.join("ro.txt")) & FILE_ATTRIBUTE_READONLY, 0);
        // The trashed read-only file can be restored over the new one.
        let run = report.trash_run.unwrap();
        crate::trash::restore(
            &f.rep,
            &run,
            &[rel("ro.txt")],
            crate::trash::OnConflict::TrashExisting,
        )
        .unwrap();
        assert_eq!(fs::read(f.rep.join("ro.txt")).unwrap(), b"x");
        assert_ne!(attrs(&f.rep.join("ro.txt")) & FILE_ATTRIBUTE_READONLY, 0);
        // Replacing it again puts a read-only file in the trash; its run can
        // still be emptied.
        write_file(&f.src, "ro.txt", b"newer", T0 + 120);
        assert_eq!(run_all(&f).failed(), 0);
        let runs = crate::trash::list_runs(&f.rep).unwrap();
        assert!(!runs.is_empty());
        for r in runs {
            crate::trash::empty_run(&f.rep, &r.id).unwrap();
        }
        assert!(crate::trash::list_runs(&f.rep).unwrap().is_empty());
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn file_and_folder_moves_rename_without_copying() {
        let f = fixture();
        write_file(&f.src, "new/a.jpg", b"aaaa", T0);
        write_file(&f.rep, "old/a.jpg", b"aaaa", T0);
        write_file(&f.src, "Keep/b.txt", b"b", T0);
        write_file(&f.rep, "Keep/sub-old/b.txt", b"b", T0);
        let report = run_all(&f);
        assert_eq!(report.failed(), 0, "{:?}", report.results);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
        assert!(report.trash_run.is_none(), "moves never trash anything");
    }

    #[test]
    fn capital_letters_only_move_renames_in_two_steps() {
        let f = fixture();
        write_file(&f.src, "photo.jpg", b"x", T0);
        write_file(&f.rep, "Photo.JPG", b"x", T0);
        let plan = crate::plan::build_plan(
            vec![Change::Move {
                from: rel("Photo.JPG"),
                to: rel("photo.jpg"),
                kind: MoveKind::File {
                    size: 1,
                    mtime_ns: ns(T0),
                },
            }],
            crate::diff::CaseMode::Insensitive,
        )
        .unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(report.applied(), 1, "{:?}", report.results);
        assert_eq!(
            read_tree(&f.rep).keys().collect::<Vec<_>>(),
            vec!["photo.jpg"]
        );
    }

    #[test]
    fn delete_goes_to_trash_unless_back_on_source() {
        let f = fixture();
        write_file(&f.rep, "gone.txt", b"1", T0);
        write_file(&f.rep, "back.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        write_file(&f.src, "back.txt", b"1", T0);
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        let outcome = |p: &str| {
            report
                .results
                .iter()
                .find(|r| r.path.as_str() == p)
                .unwrap()
                .outcome
                .clone()
        };
        assert_eq!(outcome("gone.txt"), Outcome::Applied);
        assert_eq!(
            outcome("back.txt"),
            Outcome::Skipped(SkipReason::BackOnSource)
        );
        let c = crate::trash::run_contents(&f.rep, report.trash_run.as_deref().unwrap()).unwrap();
        assert_eq!(c.items[0].path, rel("gone.txt"));
        assert!(f.rep.join("back.txt").exists());
    }

    #[test]
    fn rmdir_with_unticked_delete_is_skipped_not_failed() {
        let f = fixture();
        write_file(&f.rep, "old/a.txt", b"1", T0);
        write_file(&f.rep, "old/b.txt", b"1", T0);
        write_file(&f.src, "z.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        let approved: HashSet<ChangeId> = plan
            .changes
            .iter()
            .filter(|p| p.change.path().as_str() != "old/b.txt")
            .map(|p| p.id)
            .collect();
        let report = run_with(&f, &plan, &approved, &Control::default(), &mut |_| {});
        let rmdir = report
            .results
            .iter()
            .find(|r| r.path.as_str() == "old")
            .unwrap();
        assert_eq!(rmdir.outcome, Outcome::Skipped(SkipReason::FolderNotEmpty));
        assert_eq!(report.failed(), 0);
        assert!(f.rep.join("z.txt").exists());
        assert!(f.rep.join("old/b.txt").exists());
    }

    #[test]
    fn cancel_between_files_stops_and_keeps_finished_work() {
        let f = fixture();
        write_file(&f.src, "a.txt", b"1", T0);
        write_file(&f.src, "b.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        let control = Control::default();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &control,
            &mut |p: &Progress| {
                if p.changes_done == 1 {
                    control.cancel();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::Cancelled));
        assert_eq!(report.results.len(), 1);
        assert!(f.rep.join("a.txt").exists() && !f.rep.join("b.txt").exists());
    }

    #[test]
    fn cancel_mid_copy_leaves_no_temp_file() {
        let f = fixture();
        write_file(&f.src, "a.txt", b"12345", T0);
        let plan = plan_for(&f.src, &f.rep);
        let control = Control::default();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &control,
            &mut |p: &Progress| {
                if p.bytes_done > 0 {
                    control.cancel();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::Cancelled));
        assert!(read_tree(&f.rep).is_empty());
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn pause_blocks_until_resume() {
        let f = fixture();
        write_file(&f.src, "a.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        let control = Control::default();
        control.pause();
        let report = thread::scope(|s| {
            s.spawn(|| {
                thread::sleep(Duration::from_millis(150));
                assert!(control.is_paused());
                control.resume();
            });
            run_with(&f, &plan, &plan.actionable_ids(), &control, &mut |_| {})
        });
        assert_eq!(report.applied(), 1);
    }

    #[test]
    fn replica_disappearing_stops_the_run() {
        let f = fixture();
        write_file(&f.src, "a.txt", b"1", T0);
        write_file(&f.src, "b.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        let unplugged = f.rep.with_file_name("rep-unplugged");
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                if p.changes_done == 1 && f.rep.exists() {
                    fs::rename(&f.rep, &unplugged).unwrap();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::ReplicaDisconnected));
        assert_eq!(report.results.len(), 1);
        if cfg!(windows) {
            // Windows removes temps by path, so the moved-away root keeps b's
            // (the next scan removes it); no real name holds a partial file.
            let left = read_tree(&unplugged);
            let real: Vec<_> = left.keys().filter(|k| !k.ends_with(TEMP_SUFFIX)).collect();
            assert_eq!(real, ["a.txt"], "{left:?}");
            assert_eq!(left["a.txt"], b"1");
        } else {
            assert!(no_temp_files(&unplugged));
        }
    }

    #[test]
    fn source_disappearing_stops_before_deletes() {
        let f = fixture();
        write_file(&f.rep, "gone.txt", b"1", T0);
        write_file(&f.src, "a.txt", b"1", T0);
        let plan = plan_for(&f.src, &f.rep);
        let away = f.src.with_file_name("src-away");
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                if p.changes_done == 1 && f.src.exists() {
                    fs::rename(&f.src, &away).unwrap();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::SourceDisconnected));
        assert!(f.rep.join("gone.txt").exists());
    }

    #[test]
    fn replica_vanishing_inside_a_change_is_fatal() {
        let f = fixture();
        let missing = f.rep.join("nope");
        let fatal = |s: Stop| matches!(s, Stop::Fatal(StopReason::ReplicaDisconnected));
        assert!(fatal(replica_gone_or(
            Stop::Skip(SkipReason::AlreadyGone),
            &missing
        )));
        assert!(fatal(safety_stop(
            SafetyError::Escapes(missing.clone()),
            &missing
        )));
        assert!(matches!(
            replica_gone_or(Stop::Skip(SkipReason::AlreadyGone), &f.rep),
            Stop::Skip(SkipReason::AlreadyGone)
        ));
        assert!(matches!(
            safety_stop(SafetyError::Escapes(f.rep.clone()), &f.rep),
            Stop::Fail(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn delete_fails_on_unexpected_source_error() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        write_file(&f.src, "d/x.txt", b"1", T0);
        write_file(&f.rep, "d/x.txt", b"1", T0);
        let hidden = f.src.join("d");
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::symlink_metadata(hidden.join("x.txt")).is_ok() {
            fs::set_permissions(&hidden, fs::Permissions::from_mode(0o755)).unwrap();
            return; // running as root: permissions are not enforced
        }
        let plan = crate::plan::build_plan(
            vec![Change::Delete {
                path: rel("d/x.txt"),
                size: 1,
                mtime_ns: ns(T0),
            }],
            crate::diff::CaseMode::Sensitive,
        )
        .unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(report.results[0].outcome, Outcome::Failed(_)));
        assert!(f.rep.join("d/x.txt").exists());
    }
    #[cfg(unix)]
    #[test]
    fn a_move_whose_source_is_unreadable_fails_and_can_be_retried() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        write_file(&f.src, "new/a.jpg", b"aaaa", T0);
        write_file(&f.src, "old/keep.txt", b"k", T0);
        write_file(&f.rep, "old/a.jpg", b"aaaa", T0);
        write_file(&f.rep, "old/keep.txt", b"k", T0);
        let plan = plan_for(&f.src, &f.rep);
        assert!(
            plan.changes
                .iter()
                .any(|p| matches!(p.change, Change::Move { .. }))
        );
        let locked = f.src.join("new");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::metadata(locked.join("a.jpg")).is_ok() {
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            eprintln!("SKIPPED: running as root, permissions are not enforced");
            return;
        }
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let move_id = plan
            .changes
            .iter()
            .find(|p| matches!(p.change, Change::Move { .. }))
            .unwrap()
            .id;
        let mv = report.results.iter().find(|r| r.id == move_id).unwrap();
        assert_eq!(mv.outcome, Outcome::Failed(FailReason::PermissionDenied));
        assert!(report.failed_ids().contains(&mv.id));
        assert!(f.rep.join("old/a.jpg").exists());
    }

    #[test]
    fn mkdir_and_rmdir_recheck_the_source() {
        let f = fixture();
        fs::create_dir_all(f.src.join("empty")).unwrap();
        fs::create_dir_all(f.rep.join("gone")).unwrap();
        let plan = plan_for(&f.src, &f.rep);
        fs::remove_dir(f.src.join("empty")).unwrap();
        fs::create_dir_all(f.src.join("gone")).unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        let outcome = |p: &str| {
            report
                .results
                .iter()
                .find(|r| r.path.as_str() == p)
                .map(|r| r.outcome.clone())
        };
        assert_eq!(
            outcome("empty"),
            Some(Outcome::Skipped(SkipReason::DeletedSinceScan))
        );
        assert_eq!(
            outcome("gone"),
            Some(Outcome::Skipped(SkipReason::BackOnSource))
        );
        assert!(!f.rep.join("empty").exists());
        assert!(f.rep.join("gone").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn mkdir_skips_a_source_folder_replaced_by_a_link() {
        let f = fixture();
        fs::create_dir_all(f.src.join("empty")).unwrap();
        let plan = plan_for(&f.src, &f.rep);
        fs::remove_dir(f.src.join("empty")).unwrap();
        fs::create_dir_all(f.src.join("elsewhere")).unwrap();
        std::os::unix::fs::symlink(f.src.join("elsewhere"), f.src.join("empty")).unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(
            report.results[0].outcome,
            Outcome::Skipped(SkipReason::ChangedSincePreview)
        );
        assert!(!f.rep.join("empty").exists());
    }

    #[test]
    fn rmdir_matches_the_source_name_exactly() {
        let f = fixture();
        fs::create_dir_all(f.rep.join("gone")).unwrap();
        let plan = plan_for(&f.src, &f.rep);
        fs::create_dir_all(f.src.join("GONE")).unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        let r = report
            .results
            .iter()
            .find(|r| r.path.as_str() == "gone")
            .unwrap();
        assert_eq!(r.outcome, Outcome::Applied);
    }

    #[test]
    fn progress_ends_at_the_total_when_a_copy_is_skipped() {
        let f = fixture();
        write_file(&f.src, "a.txt", b"aaaa", T0);
        write_file(&f.src, "b.txt", b"bbbbbbbb", T0);
        let plan = plan_for(&f.src, &f.rep);
        write_file(&f.src, "b.txt", b"changed!!", T0 + 60);
        let mut last = Progress::default();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p| {
                assert!(p.bytes_done <= p.bytes_total, "{p:?}");
                last = p.clone();
            },
        );
        assert_eq!((report.applied(), report.skipped()), (1, 1));
        assert_eq!((last.bytes_done, last.bytes_total), (4, 4));
        assert_eq!(last.changes_done, 2);
    }

    #[test]
    fn a_stranded_case_rename_says_where_the_file_is() {
        let e = io::Error::other("disk said no");
        assert_eq!(
            stranded_detail(&e, &rel("Photos/a.jpg.replica-sync-rename")),
            "disk said no; the file was left at Photos/a.jpg.replica-sync-rename"
        );
    }

    #[test]
    fn retry_failed_reruns_only_the_failed_changes_of_the_same_plan() {
        let f = fixture();
        write_file(&f.src, "a/x.txt", b"x", T0);
        write_file(&f.src, "b.txt", b"b", T0);
        write_file(&f.rep, "a", b"in the way", T0); // a file where the folder must go
        let plan = crate::plan::build_plan(
            vec![
                Change::Create {
                    path: rel("a/x.txt"),
                    size: 1,
                    mtime_ns: ns(T0),
                },
                Change::Create {
                    path: rel("b.txt"),
                    size: 1,
                    mtime_ns: ns(T0),
                },
            ],
            crate::diff::CaseMode::Sensitive,
        )
        .unwrap();
        let first = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(
            (first.applied(), first.failed()),
            (1, 1),
            "{:?}",
            first.results
        );
        fs::remove_file(f.rep.join("a")).unwrap();
        let retry = run_with(
            &f,
            &plan,
            &first.failed_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(retry.results.len(), 1);
        assert_eq!(retry.results[0].path, rel("a/x.txt"));
        assert_eq!(retry.applied(), 1);
        assert_eq!(fs::read(f.rep.join("a/x.txt")).unwrap(), b"x");
    }
    #[test]
    fn case_rename_intermediate_is_a_normal_replica_entry() {
        let d = tempfile::tempdir().unwrap();
        let inter = case_rename_path(&d.path().join("Photo.JPG"));
        fs::write(&inter, b"x").unwrap();
        let s = crate::scan::scan(
            d.path(),
            &crate::rules::SkipRules::new(&[]).unwrap(),
            &crate::scan::ScanCounters::default(),
        )
        .unwrap();
        assert!(s.leftovers.is_empty(), "{:?}", s.leftovers);
        assert!(s.unlisted.is_empty(), "{:?}", s.unlisted);
        let names: Vec<_> = s.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(names, vec![inter.file_name().unwrap().to_str().unwrap()]);
    }

    #[test]
    fn capital_letters_move_refuses_a_taken_intermediate_name() {
        let f = fixture();
        write_file(&f.src, "photo.jpg", b"x", T0);
        write_file(&f.rep, "Photo.JPG", b"x", T0);
        let taken = case_rename_path(&f.rep.join("Photo.JPG"));
        fs::write(&taken, b"user data").unwrap();
        let plan = crate::plan::build_plan(
            vec![Change::Move {
                from: rel("Photo.JPG"),
                to: rel("photo.jpg"),
                kind: MoveKind::File {
                    size: 1,
                    mtime_ns: ns(T0),
                },
            }],
            crate::diff::CaseMode::Insensitive,
        )
        .unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(
            report.results[0].outcome,
            Outcome::Failed(FailReason::TargetExists)
        );
        assert_eq!(fs::read(&taken).unwrap(), b"user data");
        assert_eq!(fs::read(f.rep.join("Photo.JPG")).unwrap(), b"x");
    }

    fn file_move(from: &str, to: &str) -> Plan {
        crate::plan::build_plan(
            vec![Change::Move {
                from: rel(from),
                to: rel(to),
                kind: MoveKind::File {
                    size: 1,
                    mtime_ns: ns(T0),
                },
            }],
            crate::diff::CaseMode::Sensitive,
        )
        .unwrap()
    }

    #[test]
    fn move_refuses_when_destination_appeared_since_preview() {
        let f = fixture();
        write_file(&f.src, "b/x.txt", b"x", T0);
        write_file(&f.rep, "a/x.txt", b"x", T0);
        let plan = file_move("a/x.txt", "b/x.txt");
        write_file(&f.rep, "b/x.txt", b"appeared later", T0);
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        assert_eq!(
            report.results[0].outcome,
            Outcome::Failed(FailReason::TargetExists)
        );
        assert_eq!(fs::read(f.rep.join("a/x.txt")).unwrap(), b"x");
        assert_eq!(fs::read(f.rep.join("b/x.txt")).unwrap(), b"appeared later");
        assert!(!f.rep.join(crate::trash::TRASH_DIR).exists());
    }

    #[test]
    fn move_skips_when_source_entry_is_gone_from_the_replica() {
        let f = fixture();
        write_file(&f.src, "b/x.txt", b"x", T0);
        write_file(&f.src, "c.txt", b"c", T0);
        write_file(&f.rep, "a/x.txt", b"x", T0);
        let mut changes = vec![Change::Create {
            path: rel("c.txt"),
            size: 1,
            mtime_ns: ns(T0),
        }];
        changes.extend(
            file_move("a/x.txt", "b/x.txt")
                .changes
                .into_iter()
                .map(|p| p.change),
        );
        let plan = crate::plan::build_plan(changes, crate::diff::CaseMode::Sensitive).unwrap();
        fs::remove_file(f.rep.join("a/x.txt")).unwrap();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        let outcome = |p: &str| {
            report
                .results
                .iter()
                .find(|r| r.path.as_str() == p)
                .map(|r| r.outcome.clone())
                .unwrap()
        };
        // A move's result is recorded under its destination.
        assert_eq!(
            outcome("b/x.txt"),
            Outcome::Skipped(SkipReason::NoLongerInReplica)
        );
        assert_eq!(outcome("c.txt"), Outcome::Applied);
        assert!(!f.rep.join("b/x.txt").exists());
        assert_eq!(fs::read(f.rep.join("c.txt")).unwrap(), b"c");
    }

    #[cfg(unix)]
    #[test]
    fn read_only_replica_folder_fails_with_permission_denied_and_continues() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        write_file(&f.src, "locked/a.txt", b"a", T0);
        write_file(&f.src, "open/b.txt", b"b", T0);
        fs::create_dir_all(f.rep.join("locked")).unwrap();
        fs::create_dir_all(f.rep.join("open")).unwrap();
        let plan = plan_for(&f.src, &f.rep);
        let locked = f.rep.join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(locked.join("probe"), b"").is_ok() {
            fs::remove_file(locked.join("probe")).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            return; // running as root: permissions are not enforced
        }
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |_| {},
        );
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let outcome = |p: &str| {
            report
                .results
                .iter()
                .find(|r| r.path.as_str() == p)
                .map(|r| r.outcome.clone())
                .unwrap()
        };
        assert_eq!(
            outcome("locked/a.txt"),
            Outcome::Failed(FailReason::PermissionDenied)
        );
        assert_eq!(outcome("open/b.txt"), Outcome::Applied);
        assert!(report.stopped.is_none(), "{:?}", report.stopped);
        assert!(!locked.join("a.txt").exists());
        assert_eq!(fs::read(f.rep.join("open/b.txt")).unwrap(), b"b");
    }
}

#[cfg(all(test, any(target_os = "linux", windows)))]
mod batch_tests {
    use super::tests::{fixture, no_temp_files, run_all, run_with};
    use super::*;
    use crate::testutil::{T0, plan_for, read_tree, write_file};
    use durable::hooks;
    use std::collections::HashMap;
    use std::fs;

    /// Batching on for this test whatever the temp folder's filesystem; every
    /// hook is reset when it drops.
    struct Hooks;

    /// An I/O error and a full disk, as the OS reports them.
    #[cfg(unix)]
    const EIO: i32 = libc::EIO;
    #[cfg(windows)]
    const EIO: i32 = 1117; // ERROR_IO_DEVICE
    #[cfg(unix)]
    const ENOSPC: i32 = libc::ENOSPC;
    #[cfg(windows)]
    const ENOSPC: i32 = 112; // ERROR_DISK_FULL

    impl Drop for Hooks {
        fn drop(&mut self) {
            hooks::FORCE_BATCH.set(false);
            #[cfg(target_os = "linux")]
            hooks::OTHER_DEVICE.set(false);
            hooks::FLUSH_ERRNO.set(None);
            hooks::WRITE_ERRNO.set(None);
            crate::trash::MANIFEST_ERRNO.set(None);
        }
    }

    fn batched() -> (Hooks, super::tests::Fixture) {
        hooks::FORCE_BATCH.set(true);
        (Hooks, fixture())
    }

    fn name(i: usize) -> String {
        format!("d/f{i:02}.txt")
    }

    /// `n` small source files, `d/f00.txt`…, each holding its own name.
    fn sources(f: &super::tests::Fixture, n: usize) {
        for i in 0..n {
            write_file(&f.src, &name(i), name(i).as_bytes(), T0);
        }
    }

    fn flushes() -> u64 {
        hooks::FLUSHES.get()
    }

    /// Every real-named (non-temp) replica file holds its complete source content.
    fn no_partial_real_files(f: &super::tests::Fixture) -> bool {
        read_tree(&f.rep)
            .iter()
            .filter(|(k, _)| !k.ends_with(TEMP_SUFFIX))
            .all(|(k, v)| fs::read(f.src.join(k)).ok().as_ref() == Some(v))
    }

    #[test]
    fn a_batch_of_ten_files_renames_all_after_one_flush() {
        let (_on, f) = batched();
        sources(&f, 10);
        let before = flushes();
        let report = run_all(&f);
        assert_eq!(flushes() - before, 1, "one flush for the whole batch");
        assert_eq!(report.applied(), 10, "{:?}", report.results);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn nothing_is_renamed_before_its_batch_is_flushed() {
        let (_on, f) = batched();
        sources(&f, 10);
        let plan = plan_for(&f.src, &f.rep);
        let mut written_at: HashMap<String, u64> = HashMap::new();
        let mut seen_done = 0;
        let mut checked = 0;
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                // Whatever moment a crash or unplug hits, real names hold whole files.
                assert!(no_partial_real_files(&f));
                if p.changes_done == 0 && p.bytes_done > 0 {
                    // Still writing the batch: only temps so far.
                    assert!(read_tree(&f.rep).keys().all(|k| k.ends_with(TEMP_SUFFIX)));
                    let cur = p.current.as_ref().unwrap().as_str().to_owned();
                    written_at.entry(cur).or_insert_with(flushes);
                }
                if p.changes_done > seen_done {
                    seen_done = p.changes_done;
                    let done = name(seen_done - 1);
                    assert!(f.rep.join(&done).exists());
                    assert!(
                        flushes() > written_at[&done],
                        "{done} renamed before a flush"
                    );
                    checked += 1;
                }
            },
        );
        assert_eq!(report.applied(), 10);
        assert_eq!(checked, 10);
    }

    #[test]
    fn a_crash_mid_batch_leaves_only_temps_and_the_next_run_recovers() {
        let (_on, f) = batched();
        sources(&f, 10);
        let plan = plan_for(&f.src, &f.rep);
        let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_with(
                &f,
                &plan,
                &plan.actionable_ids(),
                &Control::default(),
                &mut |p: &Progress| {
                    if p.current.as_ref().is_some_and(|c| c.as_str() == name(6)) {
                        panic!("simulated crash");
                    }
                },
            )
        }));
        assert!(crashed.is_err());
        let left = read_tree(&f.rep);
        assert!(!left.is_empty(), "the crash left the batch's temps");
        assert!(left.keys().all(|k| k.ends_with(TEMP_SUFFIX)), "{left:?}");
        let report = run_all(&f);
        // The stranded temps are planned as deletes (to the trash), then the 10 copies.
        assert_eq!(report.applied(), 10 + left.len(), "{:?}", report.results);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn cancel_mid_batch_stops_at_once_and_drops_the_written_temps() {
        let (_on, f) = batched();
        sources(&f, 10);
        let plan = plan_for(&f.src, &f.rep);
        let control = Control::default();
        let mut opened = Vec::new();
        let before = flushes();
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &control,
            &mut |p: &Progress| {
                if let Some(c) = &p.current
                    && opened.last() != Some(c)
                {
                    opened.push(c.clone());
                }
                if p.current.as_ref().is_some_and(|c| c.as_str() == name(4)) && p.bytes_done > 0 {
                    control.cancel();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::Cancelled));
        assert!(report.results.is_empty(), "{:?}", report.results);
        assert_eq!(
            opened.last().unwrap().as_str(),
            name(4),
            "no file after the cancel"
        );
        assert_eq!(flushes(), before, "a cancel does not wait for a flush");
        assert!(read_tree(&f.rep).is_empty());
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn a_source_changing_inside_a_batch_fails_only_that_file() {
        let (_on, f) = batched();
        sources(&f, 10);
        let plan = plan_for(&f.src, &f.rep);
        fs::remove_file(f.src.join(name(7))).unwrap();
        let mut edited = false;
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                // f03's temp is complete; its source changes before the flush.
                if !edited && p.current.as_ref().is_some_and(|c| c.as_str() == name(5)) {
                    use std::io::Write;
                    fs::OpenOptions::new()
                        .append(true)
                        .open(f.src.join(name(3)))
                        .unwrap()
                        .write_all(b" more")
                        .unwrap();
                    edited = true;
                }
            },
        );
        let ids: Vec<_> = report.results.iter().map(|r| r.id).collect();
        let planned: Vec<_> = plan.changes.iter().map(|p| p.id).collect();
        assert_eq!(ids, planned, "one result per change, in plan order");
        for (i, r) in report.results.iter().enumerate() {
            assert_eq!(r.path.as_str(), name(i));
            let want = match i {
                3 => Outcome::Failed(FailReason::ChangedDuringCopy),
                7 => Outcome::Skipped(SkipReason::DeletedSinceScan),
                _ => Outcome::Applied,
            };
            assert_eq!(r.outcome, want, "{}", r.path);
        }
        assert!(!f.rep.join(name(3)).exists() && !f.rep.join(name(7)).exists());
        assert_eq!(read_tree(&f.rep).len(), 8);
        assert!(no_temp_files(&f.rep));
    }

    // Linux only: on Windows, files open under a folder normally stop the
    // folder from being renamed, so this way of "unplugging" isn't reliable
    // there; `replica_disappearing_stops_the_run` covers a moved Windows root.
    #[cfg(target_os = "linux")]
    #[test]
    fn replica_moved_away_mid_batch_stops_and_removes_its_temps() {
        let (_on, f) = batched();
        sources(&f, 10);
        let plan = plan_for(&f.src, &f.rep);
        let unplugged = f.rep.with_file_name("rep-unplugged");
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                if p.current.as_ref().is_some_and(|c| c.as_str() == name(5)) && f.rep.exists() {
                    fs::rename(&f.rep, &unplugged).unwrap();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::ReplicaDisconnected));
        // f00-f04 were waiting for the flush, so they are not done; f05 was cut short.
        let results: Vec<_> = report
            .results
            .iter()
            .map(|r| (r.path.as_str().to_owned(), r.outcome.clone()))
            .collect();
        assert_eq!(
            results,
            vec![(name(5), Outcome::Failed(FailReason::Interrupted))]
        );
        assert!(
            read_tree(&unplugged).is_empty(),
            "{:?}",
            read_tree(&unplugged)
        );
    }

    #[test]
    fn many_files_flush_in_batches_of_at_most_256() {
        let (_on, f) = batched();
        let n = durable::BATCH_FILES + 44;
        for i in 0..n {
            write_file(&f.src, &format!("m/{i:04}"), b"x", T0);
        }
        let before = flushes();
        let report = run_all(&f);
        assert_eq!(report.applied(), n);
        assert_eq!(flushes() - before, 2);
        assert!(no_temp_files(&f.rep));
    }

    fn outcomes(report: &RunReport) -> Vec<(String, Outcome)> {
        report
            .results
            .iter()
            .map(|r| (r.path.as_str().to_owned(), r.outcome.clone()))
            .collect()
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn batching_only_on_filesystems_whose_syncfs_flushes_the_disk() {
        use durable::{decide, parse_kernel};
        let new = Some((6, 8));
        for (magic, fs) in [
            (0xEF53, "ext4"),
            (0x9123_683E, "btrfs"),
            (0x5846_5342, "xfs"),
            (0xF2F5_2010, "f2fs"),
        ] {
            assert_eq!(decide(Some(magic), new), Ok(fs));
        }
        assert_eq!(libc::XFS_SUPER_MAGIC as u32, 0x5846_5342);
        for (magic, fs) in [
            (0x6573_5546, "fuse"),
            (0x4d44, "vfat"),
            (0x2011_BAB0, "exfat"),
            (0x7366_746E, "ntfs3"),
            (0x0102_1994, "tmpfs"),
            (0x1234_5678, "filesystem type 0x12345678"),
        ] {
            let why = decide(Some(magic), new).unwrap_err();
            assert!(why.starts_with(fs), "{why}");
        }
        assert!(decide(None, new).is_err(), "root not opened");
        assert!(decide(Some(0xEF53), Some((5, 7))).is_err());
        assert_eq!(decide(Some(0xEF53), Some((5, 8))), Ok("ext4"));
        assert!(decide(Some(0xEF53), None).is_err());
        assert_eq!(parse_kernel("7.2.9-201.fc44.x86_64"), Some((7, 2)));
        assert_eq!(parse_kernel("5.8"), Some((5, 8)));
        assert_eq!(parse_kernel("6.10.0-rc1"), Some((6, 10)));
        assert_eq!(parse_kernel("bogus"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_unopenable_root_or_tmpfs_syncs_each_file() {
        let d = tempfile::tempdir().unwrap();
        let gone = durable::Replica::open(&d.path().join("missing"));
        assert!(!gone.durability().batched);
        assert_eq!(gone.batch_files(), 1);
        let shm = Path::new("/dev/shm");
        if shm.is_dir() {
            let d = tempfile::tempdir_in(shm).unwrap();
            let r = durable::Replica::open(d.path());
            assert!(!r.durability().batched, "{:?}", r.durability());
            assert!(r.durability().why.starts_with("tmpfs"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_run_reports_its_durability_mode() {
        let (_on, f) = batched();
        sources(&f, 2);
        let d = run_all(&f).durability;
        assert!(d.batched && d.why == "test" && d.synced_separately == 0);
        hooks::FORCE_BATCH.set(false);
        let g = fixture();
        write_file(&g.src, "a", b"1", T0);
        let expected = durable::Replica::open(&g.rep).durability();
        assert_eq!(run_all(&g).durability, expected);
    }

    #[cfg(windows)]
    #[test]
    fn windows_runs_always_batch_and_say_how() {
        let f = fixture();
        sources(&f, 2);
        let d = run_all(&f).durability;
        assert!(d.batched && d.synced_separately == 0, "{d:?}");
        assert!(d.why.contains("FlushFileBuffers"), "{d:?}");
        assert_eq!(durable::Replica::open(&f.rep).batch_files(), 256);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn temps_on_another_filesystem_are_synced_one_by_one() {
        let (_on, f) = batched();
        sources(&f, 10);
        hooks::OTHER_DEVICE.set(true);
        let (flushed, syncs) = (flushes(), hooks::FILE_SYNCS.get());
        let report = run_all(&f);
        assert_eq!(report.applied(), 10);
        assert_eq!(hooks::FILE_SYNCS.get() - syncs, 10);
        assert_eq!(flushes(), flushed, "the root's syncfs would not cover them");
        assert_eq!(report.durability.synced_separately, 10);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn a_failed_flush_fails_every_copy_in_the_batch() {
        let (_on, f) = batched();
        sources(&f, 5);
        hooks::FLUSH_ERRNO.set(Some(("", EIO)));
        let report = run_all(&f);
        assert_eq!(report.stopped, None, "the next batch may still work");
        assert_eq!(report.results.len(), 5);
        for r in &report.results {
            assert!(
                matches!(&r.outcome, Outcome::Failed(FailReason::Io { .. })),
                "{r:?}"
            );
        }
        assert!(read_tree(&f.rep).is_empty(), "no real name, no temp");
    }

    #[test]
    fn a_disk_full_during_the_flush_interrupts_the_batch_and_stops() {
        let (_on, f) = batched();
        sources(&f, 5);
        hooks::FLUSH_ERRNO.set(Some(("", ENOSPC)));
        let report = run_all(&f);
        assert_eq!(report.stopped, Some(StopReason::ReplicaFull));
        let interrupted = Outcome::Failed(FailReason::Interrupted);
        assert_eq!(
            outcomes(&report),
            (0..5)
                .map(|i| (name(i), interrupted.clone()))
                .collect::<Vec<_>>()
        );
        assert!(read_tree(&f.rep).is_empty(), "no real name, no temp");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_replica_lost_during_the_flush_reports_the_batch_interrupted() {
        let (_on, f) = batched();
        sources(&f, 5);
        let plan = plan_for(&f.src, &f.rep);
        let unplugged = f.rep.with_file_name("rep-unplugged");
        hooks::FLUSH_ERRNO.set(Some(("", EIO)));
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                // The last temp is written; the drive goes before the flush.
                if p.bytes_done == p.bytes_total && f.rep.exists() {
                    fs::rename(&f.rep, &unplugged).unwrap();
                }
            },
        );
        assert_eq!(report.stopped, Some(StopReason::ReplicaDisconnected));
        let interrupted = Outcome::Failed(FailReason::Interrupted);
        assert_eq!(
            outcomes(&report),
            (0..5)
                .map(|i| (name(i), interrupted.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            read_tree(&unplugged).is_empty(),
            "{:?}",
            read_tree(&unplugged)
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_flush_failure_before_a_pause_fails_only_that_copy() {
        let (_on, f) = batched();
        sources(&f, 10);
        hooks::FLUSH_ERRNO.set(Some(("f01", EIO)));
        let plan = plan_for(&f.src, &f.rep);
        let control = Control::default();
        let parks = hooks::PARKS.get();
        let report = std::thread::scope(|s| {
            let mut paused = false;
            run_with(
                &f,
                &plan,
                &plan.actionable_ids(),
                &control,
                &mut |p: &Progress| {
                    if !paused
                        && p.current.as_ref().is_some_and(|c| c.as_str() == name(4))
                        && p.bytes_done > 0
                    {
                        paused = true;
                        control.pause();
                        let control = &control;
                        s.spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            control.resume();
                        });
                    }
                },
            )
        });
        assert_eq!(hooks::PARKS.get() - parks, 1);
        assert_eq!(report.stopped, None);
        for (path, outcome) in outcomes(&report) {
            if path == name(1) {
                assert!(
                    matches!(outcome, Outcome::Failed(FailReason::Io { .. })),
                    "{outcome:?}"
                );
            } else {
                assert_eq!(outcome, Outcome::Applied, "{path}");
            }
        }
        assert!(no_temp_files(&f.rep));
    }

    #[cfg(windows)]
    #[test]
    fn windows_keeps_the_temps_whose_own_flush_worked() {
        let (_on, f) = batched();
        sources(&f, 5);
        hooks::FLUSH_ERRNO.set(Some(("f02", EIO)));
        let report = run_all(&f);
        assert_eq!(report.stopped, None);
        for (path, outcome) in outcomes(&report) {
            if path == name(2) {
                assert!(
                    matches!(outcome, Outcome::Failed(FailReason::Io { .. })),
                    "{outcome:?}"
                );
            } else {
                assert_eq!(outcome, Outcome::Applied, "{path}");
            }
        }
        assert!(!f.rep.join(name(2)).exists());
        assert!(no_temp_files(&f.rep));

        // A full disk during the flush keeps the new files that did reach it.
        let g = fixture();
        sources(&g, 5);
        hooks::FLUSH_ERRNO.set(Some(("f02", ENOSPC)));
        let report = run_all(&g);
        assert_eq!(report.stopped, Some(StopReason::ReplicaFull));
        let want: Vec<_> = (0..5)
            .map(|i| {
                let o = if i == 2 {
                    Outcome::Failed(FailReason::Interrupted)
                } else {
                    Outcome::Applied
                };
                (name(i), o)
            })
            .collect();
        assert_eq!(outcomes(&report), want);
        assert_eq!(read_tree(&g.rep).len(), 4);
        assert!(no_temp_files(&g.rep));
    }

    #[test]
    fn a_pause_mid_batch_resumes_and_finishes_the_batch() {
        let (_on, f) = batched();
        sources(&f, 10);
        let plan = plan_for(&f.src, &f.rep);
        let control = Control::default();
        #[cfg(windows)]
        let parks = hooks::PARKS.get();
        let report = std::thread::scope(|s| {
            let mut paused = false;
            run_with(
                &f,
                &plan,
                &plan.actionable_ids(),
                &control,
                &mut |p: &Progress| {
                    if !paused
                        && p.current.as_ref().is_some_and(|c| c.as_str() == name(4))
                        && p.bytes_done > 0
                    {
                        paused = true;
                        control.pause();
                        let control = &control;
                        s.spawn(move || {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            control.resume();
                        });
                    }
                },
            )
        });
        // On Windows the waiting temps were flushed and closed before the pause.
        #[cfg(windows)]
        assert_eq!(hooks::PARKS.get() - parks, 1);
        assert_eq!(report.applied(), 10, "{:?}", report.results);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn a_full_disk_keeps_new_files_and_leaves_old_versions_in_place() {
        let (_on, f) = batched();
        write_file(&f.src, "a.txt", b"new a", T0);
        write_file(&f.src, "b.txt", b"new b", T0 + 60);
        write_file(&f.rep, "b.txt", b"old b", T0);
        write_file(&f.src, "c.txt", b"new c", T0);
        write_file(&f.src, "d.txt", b"new d", T0);
        hooks::WRITE_ERRNO.set(Some(("d.txt", ENOSPC)));
        let report = run_all(&f);
        assert_eq!(report.stopped, Some(StopReason::ReplicaFull));
        let interrupted = Outcome::Failed(FailReason::Interrupted);
        assert_eq!(
            outcomes(&report),
            vec![
                ("a.txt".to_owned(), Outcome::Applied),
                ("b.txt".to_owned(), interrupted.clone()),
                ("c.txt".to_owned(), Outcome::Applied),
                ("d.txt".to_owned(), interrupted),
            ]
        );
        assert_eq!(fs::read(f.rep.join("b.txt")).unwrap(), b"old b");
        assert!(report.trash_run.is_none(), "nothing was trashed");
        assert_eq!(
            read_tree(&f.rep).keys().collect::<Vec<_>>(),
            vec!["a.txt", "b.txt", "c.txt"]
        );
    }

    #[test]
    fn an_update_whose_trash_line_cannot_be_written_keeps_the_old_version() {
        let (_on, f) = batched();
        write_file(&f.src, "b.txt", b"new b", T0 + 60);
        write_file(&f.rep, "b.txt", b"old b", T0);
        crate::trash::MANIFEST_ERRNO.set(Some(ENOSPC));
        let report = run_all(&f);
        assert_eq!(report.stopped, Some(StopReason::ReplicaFull));
        assert_eq!(
            outcomes(&report),
            vec![("b.txt".to_owned(), Outcome::Failed(FailReason::Interrupted))]
        );
        assert_eq!(fs::read(f.rep.join("b.txt")).unwrap(), b"old b");
        assert!(no_temp_files(&f.rep));
        // The rolled-back move opened the run, so no empty run is left behind.
        assert!(report.trash_run.is_none(), "{:?}", report.trash_run);
        assert!(crate::trash::list_runs(&f.rep).unwrap().is_empty());
    }

    #[test]
    fn copies_are_flushed_and_renamed_before_the_deletes_that_follow() {
        let (_on, f) = batched();
        write_file(&f.src, "new/moved.bin", b"same bytes", T0);
        write_file(&f.rep, "old/moved.bin", b"same bytes", T0);
        write_file(&f.rep, "gone.txt", b"bye", T0);
        sources(&f, 3);
        let plan = plan_for(&f.src, &f.rep);
        let kinds: Vec<_> = plan
            .changes
            .iter()
            .map(|p| match &p.change {
                Change::Move { .. } => "move",
                Change::Create { .. } => "create",
                Change::Delete { .. } => "delete",
                Change::RmDir { .. } => "rmdir",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(kinds, ["move", "create", "create", "create", "delete"]);
        let before = flushes();
        let mut copies_in_place_at_delete = false;
        let report = run_with(
            &f,
            &plan,
            &plan.actionable_ids(),
            &Control::default(),
            &mut |p: &Progress| {
                if p.current.as_ref().is_some_and(|c| c.as_str() == "gone.txt") {
                    copies_in_place_at_delete =
                        (0..3).all(|i| f.rep.join(name(i)).exists()) && no_temp_files(&f.rep);
                }
            },
        );
        assert!(copies_in_place_at_delete);
        assert_eq!(flushes() - before, 1);
        let ids: Vec<_> = report.results.iter().map(|r| r.id).collect();
        let planned: Vec<_> = plan.changes.iter().map(|p| p.id).collect();
        assert_eq!(ids, planned);
        assert_eq!(report.applied(), 5, "{:?}", report.results);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
    }
}
