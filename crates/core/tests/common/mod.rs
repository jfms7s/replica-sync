#![allow(dead_code)]

use replica_sync_core::execute::{Control, ExecContext, RunReport, execute};
use replica_sync_core::rules::SkipRules;
use replica_sync_core::safety::ReplicaRoot;
use replica_sync_core::session::{Prepared, SessionCounters, prepare};
use replica_sync_core::{trash, volume};
use std::collections::BTreeMap;
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
    let rules = SkipRules::new(&rules.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    let case = volume::case_mode(rep).unwrap();
    prepare(src, rep, &rules, case, &SessionCounters::default()).unwrap()
}

pub fn apply_all(src: &Path, rep: &Path, prepared: &Prepared) -> RunReport {
    let replica = ReplicaRoot::new(rep).unwrap();
    let control = Control::default();
    let ctx = ExecContext {
        source_root: src,
        replica: &replica,
        control: &control,
        trash_stamp: trash::new_run_stamp(),
    };
    execute(
        &prepared.plan,
        &prepared.plan.actionable_ids(),
        &ctx,
        &mut |_| {},
    )
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
