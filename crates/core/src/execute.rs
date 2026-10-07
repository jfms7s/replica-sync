//! Apply the approved changes of a plan to the replica, one at a time.

use crate::model::{Change, MoveKind, RelPath, modified_ns};
use crate::plan::{ChangeId, Plan, Planned};
use crate::rules::TEMP_SUFFIX;
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
const DISCONNECTED: &str = "the backup drive was disconnected";
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
    Skipped(String),
    Failed(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct ChangeResult {
    pub id: ChangeId,
    pub path: RelPath,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum StopReason {
    Cancelled,
    Fatal(String),
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
    /// For "Retry failed": re-plan and approve only these.
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
    Skip(String),
    Fail(String),
    Fatal(String),
    Cancelled,
}

fn io_stop(e: &io::Error, replica_root: &Path) -> Stop {
    if !replica_root.is_dir() {
        return Stop::Fatal(DISCONNECTED.into());
    }
    let code = e.raw_os_error();
    if e.kind() == io::ErrorKind::StorageFull || code.is_some_and(|c| DISK_FULL_CODES.contains(&c))
    {
        return Stop::Fatal("the backup drive is full".into());
    }
    if code.is_some_and(|c| IN_USE_CODES.contains(&c)) {
        return Stop::Fail("in use by another program".into());
    }
    if e.kind() == io::ErrorKind::PermissionDenied {
        return Stop::Fail("permission denied".into());
    }
    Stop::Fail(e.to_string())
}

fn safety_stop(e: SafetyError, replica_root: &Path) -> Stop {
    match e {
        SafetyError::Escapes(p) => {
            Stop::Fail(format!("{} is outside the replica folder", p.display()))
        }
        SafetyError::Io(e) => io_stop(&e, replica_root),
    }
}

fn target(ctx: &ExecContext, rel: &RelPath) -> Result<PathBuf, Stop> {
    ctx.replica
        .target(rel)
        .map_err(|e| safety_stop(e, ctx.replica.path()))
}

fn temp_path(target: &Path) -> PathBuf {
    let mut name = target
        .file_name()
        .expect("targets have a name")
        .to_os_string();
    name.push(TEMP_SUFFIX);
    target.with_file_name(name)
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
    let mut trash = TrashWriter::new(ctx.replica.path(), ctx.trash_stamp.clone());
    let mut report = RunReport::default();
    for p in todo {
        if !ctx.control.proceed() {
            report.stopped = Some(StopReason::Cancelled);
            break;
        }
        if !ctx.replica.path().is_dir() {
            report.stopped = Some(StopReason::Fatal(DISCONNECTED.into()));
            break;
        }
        progress.current = Some(p.change.path().clone());
        on_progress(&progress);
        let outcome = match apply_one(&p.change, ctx, &mut trash, &mut progress, on_progress) {
            Ok(()) => Outcome::Applied,
            Err(Stop::Skip(r)) => Outcome::Skipped(r),
            Err(Stop::Fail(r)) => Outcome::Failed(r),
            Err(Stop::Cancelled) => {
                report.stopped = Some(StopReason::Cancelled);
                break;
            }
            Err(Stop::Fatal(r)) => {
                report.results.push(ChangeResult {
                    id: p.id,
                    path: p.change.path().clone(),
                    outcome: Outcome::Failed(r.clone()),
                });
                report.stopped = Some(StopReason::Fatal(r));
                break;
            }
        };
        report.results.push(ChangeResult {
            id: p.id,
            path: p.change.path().clone(),
            outcome,
        });
        progress.changes_done += 1;
        on_progress(&progress);
    }
    report.trash_run = trash.run_id().map(str::to_owned);
    report
}

fn apply_one(
    change: &Change,
    ctx: &ExecContext,
    trash: &mut TrashWriter,
    progress: &mut Progress,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<(), Stop> {
    match change {
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
        } => copy_file(ctx, path, *size, *mtime_ns, trash, progress, on_progress),
        Change::MkDir { path } => {
            fs::create_dir_all(target(ctx, path)?).map_err(|e| io_stop(&e, ctx.replica.path()))
        }
        Change::Move { from, to, kind } => move_entry(ctx, from, to, kind),
        Change::Delete { path, .. } => delete_file(ctx, path, trash),
        Change::RmDir { path } => remove_dir(ctx, path),
        Change::Skipped { .. } => Ok(()),
    }
}

fn copy_file(
    ctx: &ExecContext,
    rel: &RelPath,
    size: u64,
    mtime_ns: i64,
    trash: &mut TrashWriter,
    progress: &mut Progress,
    on_progress: &mut dyn FnMut(&Progress),
) -> Result<(), Stop> {
    let root = ctx.replica.path();
    let src_path = rel.to_path(ctx.source_root);
    let before = match fs::metadata(&src_path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(Stop::Skip("deleted since scan".into()));
        }
        Err(e) => return Err(io_stop(&e, root)),
    };
    if before.len() != size || modified_ns(&before) != mtime_ns {
        return Err(Stop::Skip("changed since preview".into()));
    }
    let dest = target(ctx, rel)?;
    fs::create_dir_all(dest.parent().expect("never the root")).map_err(|e| io_stop(&e, root))?;
    let tmp = temp_path(&dest);
    let result = write_temp(ctx, &src_path, &tmp, &before, progress, on_progress)
        .and_then(|()| replace_with_temp(rel, &dest, &tmp, trash, root));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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
    out.sync_all().map_err(on_io)?;
    let mtime = FileTime::from_system_time(before.modified().map_err(on_io)?);
    filetime::set_file_handle_times(&out, None, Some(mtime)).map_err(on_io)?;
    drop(out);
    let after = fs::metadata(src_path).map_err(on_io)?;
    if after.len() != before.len() || modified_ns(&after) != modified_ns(before) {
        return Err(Stop::Fail("file changed during copy".into()));
    }
    Ok(())
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
        return Err(Stop::Skip("changed since preview".into()));
    }
    let from_path = target(ctx, from)?;
    let to_path = target(ctx, to)?;
    if fs::symlink_metadata(&from_path).is_err() {
        return Err(Stop::Skip("no longer in the replica".into()));
    }
    let case_only = from.fold() == to.fold();
    if !case_only && fs::symlink_metadata(&to_path).is_ok() {
        return Err(Stop::Fail(
            "something already exists at the new location".into(),
        ));
    }
    fs::create_dir_all(to_path.parent().expect("never the root")).map_err(on_io)?;
    if case_only {
        let tmp = temp_path(&from_path);
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
    if fs::symlink_metadata(rel.to_path(ctx.source_root)).is_ok() {
        return Err(Stop::Skip("back on the source since the preview".into()));
    }
    let dest = target(ctx, rel)?;
    let md = match fs::symlink_metadata(&dest) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(Stop::Skip("already gone".into()));
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
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(Stop::Skip("already gone".into())),
        Err(e) if e.kind() == io::ErrorKind::DirectoryNotEmpty => {
            Err(Stop::Skip("folder not empty".into()))
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
        assert!(reasons.contains(&Outcome::Skipped("deleted since scan".into())));
        assert!(reasons.contains(&Outcome::Skipped("changed since preview".into())));
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
            Outcome::Failed("file changed during copy".into())
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
        assert!(
            matches!(&report.results[0].outcome, Outcome::Failed(r) if r.contains("outside the replica"))
        );
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
            Outcome::Skipped("back on the source since the preview".into())
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
        assert_eq!(rmdir.outcome, Outcome::Skipped("folder not empty".into()));
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
        assert_eq!(
            report.stopped,
            Some(StopReason::Fatal(
                "the backup drive was disconnected".into()
            ))
        );
        assert_eq!(report.results.len(), 1);
        assert!(no_temp_files(&unplugged));
    }
}
