#![allow(dead_code)]
//! Helpers for the integration tests. `src/testutil.rs` has a twin set for the
//! unit tests: integration tests can't see `#[cfg(test)]` modules.

use replica_sync_core::execute::{Control, ExecContext, Progress, RunReport, execute};
use replica_sync_core::model::CaseMode;
use replica_sync_core::plan::ChangeId;
use replica_sync_core::rules::SkipRules;
use replica_sync_core::safety::ReplicaRoot;
use replica_sync_core::session::{Prepared, SessionCounters, check_roots, prepare};
use replica_sync_core::{trash, volume};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;

pub const T0: i64 = 1_700_000_000;

pub fn write_file(root: &Path, rel: &str, content: &[u8], secs: i64) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(secs, 0)).unwrap();
}

/// Files under `root` as rel → bytes. `skip_trash` leaves out `.sync-trash`.
pub fn files(root: &Path, skip_trash: bool) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, skip_trash: bool, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.map(Result::unwrap) {
            if skip_trash && e.file_name() == trash::TRASH_DIR {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(root, &e.path(), skip_trash, out);
            } else {
                let rel = e
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(e.path()).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, skip_trash, &mut out);
    out
}

pub fn prepare_with(src: &Path, rep: &Path, rules: &[&str]) -> Prepared {
    // As the app does: refuse nested roots before the case probe writes.
    check_roots(src, rep).unwrap();
    let case = volume::case_mode(rep).unwrap();
    prepare_with_case(src, rep, rules, case)
}

/// `prepare_with` with the case mode forced, e.g. `Insensitive` on a Linux
/// tmpfs to exercise the case-folding paths.
pub fn prepare_with_case(src: &Path, rep: &Path, rules: &[&str], case: CaseMode) -> Prepared {
    let rules = SkipRules::new(&rules.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    prepare(src, rep, &rules, case, &SessionCounters::default()).unwrap()
}

pub fn apply_all(src: &Path, rep: &Path, prepared: &Prepared) -> RunReport {
    apply_some(
        src,
        rep,
        prepared,
        &prepared.plan.actionable_ids(),
        &Control::default(),
        &mut |_| {},
    )
}

/// Applies only `approved`, with the caller's `control` and progress callback.
pub fn apply_some(
    src: &Path,
    rep: &Path,
    prepared: &Prepared,
    approved: &HashSet<ChangeId>,
    control: &Control,
    on_progress: &mut dyn FnMut(&Progress),
) -> RunReport {
    let replica = ReplicaRoot::new(rep).unwrap();
    let ctx = ExecContext {
        source_root: src,
        replica: &replica,
        control,
        trash_stamp: trash::new_run_stamp(),
        drive_check: None,
    };
    execute(&prepared.plan, approved, &ctx, on_progress)
}

pub fn sync_all(src: &Path, rep: &Path) -> (Prepared, RunReport) {
    let p = prepare_with(src, rep, &[]);
    let report = apply_all(src, rep, &p);
    (p, report)
}

pub fn dirs() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let (s, r) = (d.path().join("src"), d.path().join("rep"));
    fs::create_dir_all(&s).unwrap();
    fs::create_dir_all(&r).unwrap();
    (d, s, r)
}

/// Sets `path` to `mode` and puts its previous mode back on drop, so a failed
/// assertion in a permission test can't leave an unreadable temp dir behind
/// (`tempfile` could not clean it up). Twin of `testutil::ModeGuard`.
#[cfg(unix)]
pub struct ModeGuard {
    path: std::path::PathBuf,
    previous: fs::Permissions,
}

#[cfg(unix)]
impl ModeGuard {
    pub fn set(path: &Path, mode: u32) -> ModeGuard {
        use std::os::unix::fs::PermissionsExt;
        let previous = fs::metadata(path).unwrap().permissions();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
        ModeGuard {
            path: path.to_path_buf(),
            previous,
        }
    }
}

#[cfg(unix)]
impl Drop for ModeGuard {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.path, self.previous.clone());
    }
}
