//! `apply --yes` refuses a run that would remove most of the replica.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn cli(args: &[&Path], extra: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_replica-sync-cli"));
    cmd.arg("apply").args(args).args(extra);
    cmd.output().unwrap()
}

fn files_in(dir: &Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter(|e| e.as_ref().unwrap().file_type().unwrap().is_file())
        .count()
}

fn wrong_replica() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let t = tempfile::tempdir().unwrap();
    let source = t.path().join("source");
    let replica = t.path().join("replica");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&replica).unwrap();
    fs::write(source.join("photo.jpg"), b"p").unwrap();
    for i in 0..50 {
        fs::write(replica.join(format!("unrelated{i}.txt")), b"x").unwrap();
    }
    (t, source, replica)
}

#[test]
fn apply_refuses_to_remove_most_of_the_replica_without_force() {
    let (_t, source, replica) = wrong_replica();
    let out = cli(&[&source, &replica], &["--yes"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("50 of the replica's 50 files"), "{err}");
    assert!(err.contains("--force"), "{err}");
    assert_eq!(files_in(&replica), 50);
    assert!(!replica.join("photo.jpg").exists());
    assert!(!replica.join(".sync-trash").exists());
}

#[test]
fn apply_with_force_applies() {
    let (_t, source, replica) = wrong_replica();
    let out = cli(&[&source, &replica], &["--yes", "--force"]);
    assert!(out.status.success(), "{out:?}");
    assert!(replica.join("photo.jpg").exists());
    assert_eq!(files_in(&replica), 1);
    assert!(replica.join(".sync-trash").exists());
}

#[test]
fn apply_without_a_guard_needs_no_force() {
    let (_t, source, replica) = wrong_replica();
    let t0 = filetime::FileTime::from_unix_time(1_700_000_000, 0);
    for i in 0..50 {
        let name = format!("unrelated{i}.txt");
        fs::write(source.join(&name), b"x").unwrap();
        filetime::set_file_mtime(source.join(&name), t0).unwrap();
        filetime::set_file_mtime(replica.join(&name), t0).unwrap();
    }
    let out = cli(&[&source, &replica], &["--yes"]);
    assert!(out.status.success(), "{out:?}");
    assert!(replica.join("photo.jpg").exists());
    assert_eq!(files_in(&replica), 51);
}
