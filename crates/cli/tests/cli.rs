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
    assert!(
        text.contains(" ms · scan replica ") && text.contains("scan source "),
        "{text}"
    );
}

/// The tree's layout, contents and times are fixed (the bench compares
/// trees made by different builds).
#[test]
fn gen_tree_makes_the_same_tree_every_time() {
    let t = tempfile::tempdir().unwrap();
    let out = cli(&[
        &"gen-tree",
        &t.path().join("g"),
        &"--files",
        &"2001",
        &"--large",
        &"0",
    ]);
    assert!(out.status.success(), "{out:?}");
    let snap = snapshot(&t.path().join("g"));
    assert_eq!(snap.len(), 2001);
    let at = |secs: u64| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    let sep = std::path::MAIN_SEPARATOR;
    assert_eq!(
        snap[0],
        (
            format!("d0000{sep}f0000000.txt"),
            b"0".to_vec(),
            at(1_700_000_000)
        )
    );
    assert_eq!(
        snap[1999],
        (
            format!("d0001{sep}f0001999.txt"),
            b"1999".to_vec(),
            at(1_700_000_999)
        )
    );
    assert_eq!(
        snap[2000],
        (
            format!("d0002{sep}f0002000.txt"),
            b"2000".to_vec(),
            at(1_700_000_000)
        )
    );
}

#[cfg(unix)]
#[test]
fn gen_tree_errors_name_the_path() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let locked = t.path().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(locked.join("probe"), b"x").is_ok() {
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        panic!("permission tests must not run as root (permissions are not enforced)");
    }
    let out = cli(&[&"gen-tree", &locked.join("tree"), &"--files", &"1"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(&*locked.join("tree").to_string_lossy()),
        "{err}"
    );
}
