//! A filesystem mounted inside the replica: the root's `syncfs` never flushes
//! it, so copies landing there must be synced one by one.
//!
//! Needs Linux, unprivileged user namespaces (`unshare -rm`) and a target
//! folder on a filesystem that batches; it skips (with a note) otherwise. The
//! parent re-runs this test binary inside a private mount namespace with a
//! tmpfs mounted at `rep/inner`.
#![cfg(target_os = "linux")]

mod common;

use common::*;
use std::path::{Path, PathBuf};

const CHILD_ENV: &str = "REPLICA_SYNC_NESTED_MOUNT_DIR";

#[test]
fn copies_onto_a_mount_inside_the_replica_are_synced_one_by_one() {
    use std::process::Command;
    let d = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let probe = d.path().join("probe");
    std::fs::create_dir(&probe).unwrap();
    if !Command::new("unshare")
        .args(["-rm", "mount", "-t", "tmpfs", "tmpfs"])
        .arg(&probe)
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("skipped: cannot mount in an unprivileged user namespace");
        return;
    }
    let (src, rep) = (d.path().join("src"), d.path().join("rep"));
    for i in 0..5 {
        write_file(&src, &format!("top/f{i}.txt"), b"on the replica disk", T0);
        write_file(&src, &format!("inner/g{i}.txt"), b"on the nested mount", T0);
    }
    std::fs::create_dir_all(rep.join("inner")).unwrap();
    let status = Command::new("unshare")
        .args([
            "-rm",
            "sh",
            "-c",
            r#"mount -t tmpfs tmpfs "$1" && exec "$2" --exact "$3" --ignored --nocapture"#,
            "sh",
        ])
        .arg(rep.join("inner"))
        .arg(std::env::current_exe().unwrap())
        .arg("nested_mount_child")
        .env(CHILD_ENV, d.path())
        .status()
        .unwrap();
    assert!(status.success(), "the child run failed");
}

/// Runs inside the namespace set up above.
#[test]
#[ignore = "run by copies_onto_a_mount_inside_the_replica_are_synced_one_by_one"]
fn nested_mount_child() {
    use std::os::unix::fs::MetadataExt;
    let Some(dir) = std::env::var_os(CHILD_ENV).map(PathBuf::from) else {
        return;
    };
    let (src, rep) = (dir.join("src"), dir.join("rep"));
    let dev = |p: &Path| std::fs::metadata(p).unwrap().dev();
    assert_ne!(dev(&rep), dev(&rep.join("inner")), "tmpfs is mounted");
    let (_, report) = sync_all(&src, &rep);
    eprintln!("{:?}", report.durability);
    if !report.durability.batched {
        eprintln!("skipped: {} does not batch", report.durability.why);
        return;
    }
    assert_eq!(report.applied(), 10, "{:?}", report.results);
    assert_eq!(report.durability.synced_separately, 5);
    assert_eq!(files(&rep, true), files(&src, false));
}
