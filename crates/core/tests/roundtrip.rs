mod common;

use common::*;
use replica_sync_core::model::{Change, MoveKind, RelPath};
#[cfg(windows)]
use replica_sync_core::reason::FailReason;
use replica_sync_core::trash::{self, OnConflict};
use std::fs;

#[test]
fn full_sync_mirrors_and_second_scan_is_empty() {
    let (_d, s, r) = dirs();
    write_file(&s, "Photos/2026/a.jpg", b"aaaa", T0);
    write_file(&s, "Docs/budget.xlsx", b"v2", T0 + 60);
    write_file(&r, "Docs/budget.xlsx", b"v1", T0);
    write_file(&r, "Temp/old.txt", b"bye", T0);
    let (_, report) = sync_all(&s, &r);
    assert_eq!(
        (report.failed(), report.stopped),
        (0, None),
        "{:?}",
        report.results
    );
    assert_eq!(files(&r, true), files(&s, true));
    assert!(prepare_with(&s, &r, &[]).plan.changes.is_empty());
    let run = report.trash_run.unwrap();
    let c = trash::run_contents(&r, &run).unwrap();
    let mut trashed: Vec<_> = c.items.iter().map(|i| i.path.to_string()).collect();
    trashed.sort();
    assert_eq!(trashed, vec!["Docs/budget.xlsx", "Temp/old.txt"]);
    // restore round-trip for the deleted file
    trash::restore(
        &r,
        &run,
        &[RelPath::new("Temp/old.txt").unwrap()],
        OnConflict::Refuse,
    )
    .unwrap();
    assert_eq!(fs::read(r.join("Temp/old.txt")).unwrap(), b"bye");
}

#[test]
fn moved_folder_is_renamed_not_copied() {
    let (_d, s, r) = dirs();
    write_file(&s, "Keep/k.txt", b"k", T0);
    write_file(&r, "Keep/k.txt", b"k", T0);
    write_file(&s, "2025/Trip/a.jpg", &vec![7u8; 4096], T0);
    write_file(&r, "Old/Trip/a.jpg", &vec![7u8; 4096], T0);
    let p = prepare_with(&s, &r, &[]);
    assert!(p.plan.changes.iter().any(|c| matches!(
        c.change,
        Change::Move {
            kind: MoveKind::Dir { files: 1, .. },
            ..
        }
    )));
    #[cfg(unix)]
    let inode =
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(r.join("Old/Trip/a.jpg")).unwrap());
    let report = apply_all(&s, &r, &p);
    assert_eq!(report.failed(), 0, "{:?}", report.results);
    assert_eq!(files(&r, true), files(&s, true));
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(r.join("2025/Trip/a.jpg")).unwrap()),
        inode
    );
}

#[test]
fn skip_rules_protect_replica_copies() {
    let (_d, s, r) = dirs();
    write_file(&r, "Temp/scratch.txt", b"mine", T0);
    let p = prepare_with(&s, &r, &["Temp/"]);
    assert!(p.plan.changes.is_empty());
}

#[test]
fn interrupted_trash_move_shows_as_unrecorded() {
    let (_d, _s, r) = dirs();
    let run = "2026-10-07_120000";
    write_file(
        &r.join(trash::TRASH_DIR).join(run).join(trash::FILES_DIR),
        "lost.jpg",
        b"x",
        T0,
    );
    let c = trash::run_contents(&r, run).unwrap();
    assert_eq!(c.unrecorded, vec![RelPath::new("lost.jpg").unwrap()]);
}

#[test]
fn long_paths_sync() {
    let (_d, s, r) = dirs();
    let deep = (0..12)
        .map(|i| format!("folder-with-a-long-name-{i:02}"))
        .collect::<Vec<_>>()
        .join("/");
    let (one, two) = (format!("{deep}/file.txt"), format!("{deep}/other.txt"));
    assert!(s.join(&one).as_os_str().len() > 300);
    write_file(&s, &one, b"deep", T0);
    write_file(&s, &two, b"also deep", T0);
    // The second Create lands in a folder that already exists with a long path.
    let (_, report) = sync_all(&s, &r);
    assert_eq!(report.failed(), 0, "{:?}", report.results);
    assert_eq!(fs::read(r.join(&one)).unwrap(), b"deep");
    assert_eq!(fs::read(r.join(&two)).unwrap(), b"also deep");
    // An Update replaces a file whose existing parent has a long path.
    write_file(&s, &two, b"deeper still", T0 + 60);
    let (p, report) = sync_all(&s, &r);
    assert!(
        p.plan
            .changes
            .iter()
            .any(|c| matches!(c.change, Change::Update { .. }))
    );
    assert_eq!(
        (report.applied(), report.failed()),
        (1, 0),
        "{:?}",
        report.results
    );
    assert_eq!(fs::read(r.join(&two)).unwrap(), b"deeper still");
    assert!(prepare_with(&s, &r, &[]).plan.changes.is_empty());
}

#[cfg(unix)]
#[test]
fn read_only_source_syncs_without_writing_to_it() {
    use std::os::unix::fs::PermissionsExt;
    let (_d, s, r) = dirs();
    write_file(&s, "a/b.txt", b"1", T0);
    write_file(&r, "gone.txt", b"1", T0);
    let set = |mode| {
        for p in [s.join("a/b.txt"), s.join("a"), s.clone()] {
            fs::set_permissions(
                &p,
                fs::Permissions::from_mode(if p.is_dir() { mode | 0o111 } else { mode }),
            )
            .unwrap();
        }
    };
    set(0o444);
    let (_, report) = sync_all(&s, &r);
    set(0o644);
    assert_eq!(report.failed(), 0, "{:?}", report.results);
    assert_eq!(files(&r, true), files(&s, true));
}

#[cfg(unix)]
#[test]
fn unreadable_source_folder_keeps_replica() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let (_d, s, r) = dirs();
    write_file(&s, "Docs/a.txt", b"1", T0);
    write_file(&r, "Docs/a.txt", b"1", T0);
    write_file(&r, "Docs/only-on-replica.txt", b"precious", T0);
    fs::set_permissions(s.join("Docs"), fs::Permissions::from_mode(0o000)).unwrap();
    let (p, report) = sync_all(&s, &r);
    fs::set_permissions(s.join("Docs"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        p.plan
            .changes
            .iter()
            .any(|c| matches!(&c.change, Change::Skipped { path, .. } if path.as_str() == "Docs"))
    );
    assert_eq!(report.results.len(), 0);
    assert_eq!(
        fs::read(r.join("Docs/only-on-replica.txt")).unwrap(),
        b"precious"
    );
}

#[cfg(windows)]
#[test]
fn locked_source_file_fails_with_in_use() {
    use replica_sync_core::execute::Outcome;
    use std::os::windows::fs::OpenOptionsExt;
    let (_d, s, r) = dirs();
    write_file(&s, "locked.txt", b"1", T0);
    let _lock = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(s.join("locked.txt"))
        .unwrap();
    let (_, report) = sync_all(&s, &r);
    assert_eq!(
        report.results[0].outcome,
        Outcome::Failed(FailReason::InUse)
    );
}

#[test]
fn folder_with_a_leftover_temp_file_clears_in_one_sync() {
    let (_d, s, r) = dirs();
    write_file(&s, "keep.txt", b"k", T0);
    write_file(&r, "keep.txt", b"k", T0);
    write_file(&r, "Trip/a.jpg", b"a", T0);
    write_file(&r, "Trip/b.jpg.replica-sync.tmp", b"partial", T0);
    let (_, report) = sync_all(&s, &r);
    assert_eq!(report.failed(), 0, "{:?}", report.results);
    assert!(!r.join("Trip").exists());
    assert!(prepare_with(&s, &r, &[]).plan.changes.is_empty());
}
