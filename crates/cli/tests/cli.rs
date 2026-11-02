//! The CLI harness: its `--yes` gate, root checks and the commands the
//! nightly bench runs (`gen-tree`, `plan --summary`).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cli(args: &[&dyn AsRef<std::ffi::OsStr>]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_replica-sync-cli"));
    for a in args {
        cmd.arg(a);
    }
    cmd.output().unwrap()
}

/// Every file under `root` as (relative path, bytes, mtime), sorted.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, std::time::SystemTime)> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>, std::time::SystemTime)>) {
        for e in fs::read_dir(dir).unwrap().map(Result::unwrap) {
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                let md = fs::metadata(&p).unwrap();
                out.push((rel, fs::read(&p).unwrap(), md.modified().unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[test]
fn apply_without_yes_exits_2_and_changes_nothing() {
    let t = tempfile::tempdir().unwrap();
    let (s, r) = (t.path().join("s"), t.path().join("r"));
    fs::create_dir_all(&s).unwrap();
    fs::create_dir_all(&r).unwrap();
    fs::write(s.join("new.txt"), b"n").unwrap();
    fs::write(r.join("old.txt"), b"o").unwrap();
    let before = snapshot(&r);
    let out = cli(&[&"apply", &s, &r]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert_eq!(snapshot(&r), before);
}

#[test]
fn plan_refuses_nested_roots_before_writing_anything() {
    let t = tempfile::tempdir().unwrap();
    let s = t.path().join("s");
    let r = s.join("backup");
    fs::create_dir_all(&r).unwrap();
    fs::write(s.join("a.txt"), b"a").unwrap();
    let out = cli(&[&"plan", &s, &r]);
    assert!(!out.status.success(), "{out:?}");
    assert_eq!(
        fs::read_dir(&r).unwrap().count(),
        0,
        "nothing written in the replica"
    );
}

#[test]
fn gen_tree_refuses_a_non_empty_folder() {
    let t = tempfile::tempdir().unwrap();
    fs::write(t.path().join("keep.txt"), b"k").unwrap();
    let out = cli(&[&"gen-tree", &t.path(), &"--files", &"5", &"--large", &"0"]);
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not empty"));
    assert_eq!(fs::read_dir(t.path()).unwrap().count(), 1);
}

#[test]
fn gen_tree_copies_plan_as_in_sync() {
    let t = tempfile::tempdir().unwrap();
    let (a, b) = (t.path().join("a"), t.path().join("b"));
    for d in [&a, &b] {
        let out = cli(&[&"gen-tree", d, &"--files", &"20", &"--large", &"0"]);
        assert!(out.status.success(), "{out:?}");
    }
    assert_eq!(snapshot(&a).len(), 20);
    let out = cli(&[&"plan", &a, &b, &"--summary"]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("source 20 files"), "{text}");
    assert!(
        text.contains("plan: 0 create, 0 update, 0 move, 0 delete, 0 folders"),
        "{text}"
    );
}
