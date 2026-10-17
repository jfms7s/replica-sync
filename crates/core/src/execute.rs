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
use crate::safety::{ReplicaRoot, SafetyError};
use crate::trash::{TrashReason, TrashWriter};
use filetime::FileTime;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::thread;
use std::time::Duration;

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
}

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
        trash: TrashWriter::new(ctx.replica.path(), ctx.trash_stamp.clone()),
        report: RunReport::default(),
        batch: Batch::default(),
        progress: &mut progress,
        on_progress,
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
        if (copy.is_none() || run.batch.is_full()) && !run.flush() {
            break;
        }
        if let Some(stop) = run.check_roots() {
            run.stop(stop);
            break;
        }
        run.progress.current = Some(p.change.path().clone());
        (run.on_progress)(run.progress);
        let result = match copy {
            Some((rel, size, mtime_ns)) => run.write_copy(p.id, rel, size, mtime_ns),
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
                // A full disk still holds the batch's whole temps: keep them.
                if r != StopReason::ReplicaFull || run.flush() {
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
        run.flush();
    }
    let mut report = run.report;
    report.trash_run = run.trash.run_id().map(str::to_owned);
    report
}

/// How many copies may wait in one [`Batch`] (written, not yet flushed and
/// renamed). Only Linux has a one-call flush for many files (`syncfs`); elsewhere
/// each temp is synced on its own and a batch holds one copy, as before.
#[cfg(target_os = "linux")]
const BATCH_FILES: usize = 256;
#[cfg(not(target_os = "linux"))]
const BATCH_FILES: usize = 1;
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
    fn is_full(&self) -> bool {
        self.items.len() >= BATCH_FILES || self.bytes >= BATCH_BYTES
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
            None
        }
    }

    /// Reports one finished change and counts it as done.
    fn finish(&mut self, id: ChangeId, path: &RelPath, outcome: Outcome) {
        self.report.results.push(ChangeResult {
            id,
            path: path.clone(),
            outcome,
        });
        self.progress.changes_done += 1;
        (self.on_progress)(self.progress);
    }

    /// Reports the change a fatal stop cut short; it never counts as done.
    fn interrupted(&mut self, id: ChangeId, path: &RelPath) {
        self.report.results.push(ChangeResult {
            id,
            path: path.clone(),
            outcome: Outcome::Failed(FailReason::Interrupted),
        });
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
                Pending::Written(w) => self.disk.remove_temp(self.ctx.replica.path(), &w.tmp),
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
    ) -> Result<(), Stop> {
        let ctx = self.ctx;
        let root = ctx.replica.path();
        let src_path = rel.to_path(ctx.source_root);
        let before = match fs::metadata(&src_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Stop::Skip(SkipReason::DeletedSinceScan));
            }
            Err(e) => return Err(io_stop(&e, root)),
        };
        if before.len() != size || modified_ns(&before) != mtime_ns {
            return Err(Stop::Skip(SkipReason::ChangedSincePreview));
        }
        let dest = target(ctx, rel)?;
        fs::create_dir_all(dest.parent().expect("never the root"))
            .map_err(|e| io_stop(&e, root))?;
        let tmp = temp_path(&dest);
        if let Err(stop) = write_temp(
            ctx,
            &src_path,
            &tmp,
            &before,
            self.progress,
            self.on_progress,
        ) {
            self.disk.remove_temp(root, &tmp);
            return Err(stop);
        }
        self.batch.bytes += before.len();
        self.batch.items.push(Pending::Written(Written {
            id,
            rel: rel.clone(),
            dest,
            tmp,
            src_path,
            len: before.len(),
            mtime_ns,
        }));
        Ok(())
    }

    /// Makes the batch durable with one flush, then renames its temps into place
    /// in plan order, re-checking each source first. False once the run stopped.
    fn flush(&mut self) -> bool {
        let root = self.ctx.replica.path();
        let has_written = self
            .batch
            .items
            .iter()
            .any(|i| matches!(i, Pending::Written(_)));
        let mut sync_failed = None;
        if has_written && let Err(e) = self.disk.flush(root) {
            match io_stop(&e, root) {
                Stop::Fatal(r) => {
                    self.stop(r);
                    return false;
                }
                Stop::Fail(r) => sync_failed = Some(r),
                Stop::Skip(_) | Stop::Cancelled => unreachable!("io_stop only fails"),
            }
        }
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
            let result = match &sync_failed {
                Some(r) => Err(Stop::Fail(r.clone())),
                None => match self.check_roots() {
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
                    self.interrupted(w.id, &w.rel);
                    self.batch.items = items.collect();
                    self.stop(r);
                    return false;
                }
            };
            if outcome != Outcome::Applied {
                self.disk.remove_temp(root, &w.tmp);
            }
            self.finish(w.id, &w.rel, outcome);
        }
        true
    }

    /// Renames one durable temp into place, unless its source changed since it
    /// was first read.
    fn commit(&mut self, w: &Written) -> Result<(), Stop> {
        let root = self.ctx.replica.path();
        let after = match fs::metadata(&w.src_path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(Stop::Skip(SkipReason::DeletedSinceScan));
            }
            Err(e) => return Err(io_stop(&e, root)),
        };
        if after.len() != w.len || modified_ns(&after) != w.mtime_ns {
            return Err(Stop::Fail(FailReason::ChangedDuringCopy));
        }
        replace_with_temp(&w.rel, &w.dest, &w.tmp, &mut self.trash, root)
    }
}

fn apply_one(change: &Change, ctx: &ExecContext, trash: &mut TrashWriter) -> Result<(), Stop> {
    match change {
        Change::Create { .. } | Change::Update { .. } => {
            unreachable!("copies go through Run::write_copy")
        }
        Change::MkDir { path } => {
            fs::create_dir_all(target(ctx, path)?).map_err(|e| io_stop(&e, ctx.replica.path()))
        }
        Change::Move { from, to, kind } => move_entry(ctx, from, to, kind),
        Change::Delete { path, .. } => delete_file(ctx, path, trash),
        Change::RmDir { path } => remove_dir(ctx, path),
        Change::Skipped { .. } => Ok(()),
    }
}

fn write_temp(
    ctx: &ExecContext,
    src_path: &Path,
    tmp: &Path,
    before: &fs::Metadata,
    progress: &mut Progress,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<(), Stop> {
    let root = ctx.replica.path();
    let on_io = |e: io::Error| io_stop(&e, root);
    let mut src = File::open(src_path).map_err(on_io)?;
    match fs::remove_file(tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(on_io(e)),
    }
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(on_io)?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = src.read(&mut buf).map_err(on_io)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(on_io)?;
        progress.bytes_done += n as u64;
        on_progress(progress);
        if !ctx.control.proceed() {
            return Err(Stop::Cancelled);
        }
    }
    if durable::SYNC_EACH_FILE {
        out.sync_all().map_err(on_io)?;
    }
    let mtime = FileTime::from_system_time(before.modified().map_err(on_io)?);
    filetime::set_file_handle_times(&out, None, Some(mtime)).map_err(on_io)?;
    Ok(())
}

/// Durability for the replica disk.
#[cfg(target_os = "linux")]
mod durable {
    use std::ffi::CString;
    use std::fs::{self, File};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// One `syncfs` per batch makes every temp in it durable.
    pub const SYNC_EACH_FILE: bool = false;

    #[cfg(test)]
    thread_local! {
        /// How many batch flushes this thread has run (tests check flush-before-rename).
        pub static FLUSHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    /// An open handle on the replica root: `syncfs` needs a descriptor on the
    /// disk, and temps can still be removed through it after the root folder is
    /// moved or renamed.
    pub struct Replica(Option<File>);

    impl Replica {
        pub fn open(root: &Path) -> Replica {
            Replica(File::open(root).ok())
        }

        pub fn flush(&self, root: &Path) -> io::Result<()> {
            let opened;
            let file = match &self.0 {
                Some(f) => f,
                None => {
                    opened = File::open(root)?;
                    &opened
                }
            };
            #[cfg(test)]
            FLUSHES.with(|n| n.set(n.get() + 1));
            // SAFETY: `file` is an open descriptor for the duration of the call.
            if unsafe { libc::syncfs(file.as_raw_fd()) } == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        }

        /// Best effort: a temp that cannot be removed is a leftover the next scan
        /// deletes.
        pub fn remove_temp(&self, root: &Path, tmp: &Path) {
            let (Some(dir), Ok(rel)) = (&self.0, tmp.strip_prefix(root)) else {
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
}

/// Durability for the replica disk: each temp is synced as it is written.
#[cfg(not(target_os = "linux"))]
mod durable {
    use std::fs;
    use std::io;
    use std::path::Path;

    pub const SYNC_EACH_FILE: bool = true;

    pub struct Replica;

    impl Replica {
        pub fn open(_root: &Path) -> Replica {
            Replica
        }

        pub fn flush(&self, _root: &Path) -> io::Result<()> {
            Ok(())
        }

        pub fn remove_temp(&self, _root: &Path, tmp: &Path) {
            let _ = fs::remove_file(tmp);
        }
    }
}

fn replace_with_temp(
    rel: &RelPath,
    dest: &Path,
    tmp: &Path,
    trash: &mut TrashWriter,
    root: &Path,
) -> Result<(), Stop> {
    match fs::symlink_metadata(dest) {
        Ok(old) => trash
            .move_in(rel, old.len(), modified_ns(&old), TrashReason::Replaced)
            .map_err(|e| io_stop(&e, root))?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_stop(&e, root)),
    }
    fs::rename(tmp, dest).map_err(|e| io_stop(&e, root))
}

fn move_entry(
    ctx: &ExecContext,
    from: &RelPath,
    to: &RelPath,
    kind: &MoveKind,
) -> Result<(), Stop> {
    let root = ctx.replica.path();
    let on_io = |e: io::Error| io_stop(&e, root);
    let still_planned = match (kind, fs::metadata(to.to_path(ctx.source_root))) {
        (MoveKind::File { size, mtime_ns }, Ok(m)) => {
            m.is_file() && m.len() == *size && modified_ns(&m) == *mtime_ns
        }
        (MoveKind::Dir { .. }, Ok(m)) => m.is_dir(),
        (_, Err(_)) => false,
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
    fs::create_dir_all(to_path.parent().expect("never the root")).map_err(on_io)?;
    if case_only {
        let tmp = case_rename_path(&from_path);
        if fs::symlink_metadata(&tmp).is_ok() {
            return Err(Stop::Fail(FailReason::TargetExists));
        }
        fs::rename(&from_path, &tmp).map_err(on_io)?;
        fs::rename(&tmp, &to_path).map_err(|e| {
            let _ = fs::rename(&tmp, &from_path);
            io_stop(&e, root)
        })
    } else {
        fs::rename(&from_path, &to_path).map_err(on_io)
    }
}

fn delete_file(ctx: &ExecContext, rel: &RelPath, trash: &mut TrashWriter) -> Result<(), Stop> {
    let root = ctx.replica.path();
    match fs::symlink_metadata(rel.to_path(ctx.source_root)) {
        Ok(_) => return Err(Stop::Skip(SkipReason::BackOnSource)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(Stop::Fail(FailReason::Io {
                detail: e.to_string(),
            }));
        }
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
    match fs::remove_dir(target(ctx, rel)?) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(replica_gone_or(
            Stop::Skip(SkipReason::AlreadyGone),
            ctx.replica.path(),
        )),
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            Err(Stop::Skip(SkipReason::FolderNotEmpty))
        }
        Err(e) => Err(io_stop(&e, ctx.replica.path())),
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
    fn stale_temp_symlink_is_not_followed() {
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
        assert_eq!(report.applied(), 1);
        assert_eq!(fs::read(&victim).unwrap(), b"outside");
        assert_eq!(fs::read(f.rep.join("a.txt")).unwrap(), b"inside");
    }
}

#[cfg(test)]
mod apply_tests {
    use super::tests::{fixture, no_temp_files, run_all, run_with};
    use super::*;
    use crate::testutil::{T0, ns, plan_for, read_tree, rel, write_file};
    use std::fs;

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
        assert!(no_temp_files(&unplugged));
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
}

#[cfg(all(test, target_os = "linux"))]
mod batch_tests {
    use super::tests::{fixture, no_temp_files, run_all, run_with};
    use super::*;
    use crate::testutil::{T0, plan_for, read_tree, write_file};
    use std::collections::HashMap;
    use std::fs;

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
        durable::FLUSHES.with(|n| n.get())
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
        let f = fixture();
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
        let f = fixture();
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
        let f = fixture();
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
        assert_eq!(report.applied(), 10, "{:?}", report.results);
        assert_eq!(read_tree(&f.rep), read_tree(&f.src));
        assert!(no_temp_files(&f.rep));
    }

    #[test]
    fn cancel_mid_batch_stops_at_once_and_drops_the_written_temps() {
        let f = fixture();
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
        let f = fixture();
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

    #[test]
    fn replica_moved_away_mid_batch_stops_and_removes_its_temps() {
        let f = fixture();
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
        let f = fixture();
        let n = BATCH_FILES + 44;
        for i in 0..n {
            write_file(&f.src, &format!("m/{i:04}"), b"x", T0);
        }
        let before = flushes();
        let report = run_all(&f);
        assert_eq!(report.applied(), n);
        assert_eq!(flushes() - before, 2);
        assert!(no_temp_files(&f.rep));
    }
}
