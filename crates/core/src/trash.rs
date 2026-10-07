//! `.sync-trash`: where every deleted or replaced replica file goes, recorded
//! one manifest line at a time so a crash never loses track of a file.

use crate::model::{RelPath, modified_ns};
use crate::safety::{ReplicaRoot, SafetyError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const TRASH_DIR: &str = ".sync-trash";
pub const FILES_DIR: &str = "files";
pub const MANIFEST: &str = "manifest.jsonl";
const STAMP_FORMAT: &str = "%Y-%m-%d_%H%M%S";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrashReason {
    Deleted,
    Replaced,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrashItem {
    pub path: RelPath,
    pub size: u64,
    pub mtime_ns: i64,
    pub reason: TrashReason,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ManifestLine {
    Item(TrashItem),
    Restored { restored: RelPath },
}

pub fn new_run_stamp() -> String {
    chrono::Local::now().format(STAMP_FORMAT).to_string()
}

pub struct TrashWriter {
    replica_root: PathBuf,
    stamp: String,
    run: Option<(String, PathBuf, File)>,
}

impl TrashWriter {
    pub fn new(replica_root: &Path, stamp: String) -> TrashWriter {
        TrashWriter {
            replica_root: replica_root.to_path_buf(),
            stamp,
            run: None,
        }
    }

    pub fn run_id(&self) -> Option<&str> {
        self.run.as_ref().map(|(id, ..)| id.as_str())
    }

    /// Renames `replica_root/rel` into this run and appends its manifest line.
    pub fn move_in(
        &mut self,
        rel: &RelPath,
        size: u64,
        mtime_ns: i64,
        reason: TrashReason,
    ) -> io::Result<()> {
        if self.run.is_none() {
            self.run = Some(self.open_run()?);
        }
        let (_, run_dir, manifest) = self.run.as_mut().expect("opened above");
        let dest = rel.to_path(&run_dir.join(FILES_DIR));
        fs::create_dir_all(dest.parent().expect("a trashed path is never the root"))?;
        if fs::symlink_metadata(&dest).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{rel} is already in trash run {run_dir:?}"),
            ));
        }
        fs::rename(rel.to_path(&self.replica_root), &dest)?;
        let line = serde_json::to_string(&ManifestLine::Item(TrashItem {
            path: rel.clone(),
            size,
            mtime_ns,
            reason,
        }))
        .map_err(io::Error::other)?;
        writeln!(manifest, "{line}")?;
        manifest.sync_data()
    }

    fn open_run(&self) -> io::Result<(String, PathBuf, File)> {
        let base = self.replica_root.join(TRASH_DIR);
        fs::create_dir_all(&base)?;
        hide(&base);
        let (mut id, mut n) = (self.stamp.clone(), 1);
        loop {
            let dir = base.join(&id);
            match fs::create_dir(&dir) {
                Ok(()) => {
                    let manifest = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join(MANIFEST))?;
                    return Ok((id, dir, manifest));
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    n += 1;
                    id = format!("{}-{n}", self.stamp);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(windows)]
fn hide(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_HIDDEN, SetFileAttributesW};
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `name` is NUL-terminated and outlives the call.
    unsafe { SetFileAttributesW(name.as_ptr(), FILE_ATTRIBUTE_HIDDEN) };
}

#[cfg(not(windows))]
fn hide(_: &Path) {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrashRunInfo {
    pub id: String,
    pub files: usize,
    pub bytes: u64,
    pub unrecorded: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TrashRunContents {
    pub items: Vec<TrashItem>,
    pub unrecorded: Vec<RelPath>,
    pub bytes: u64,
}

fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '_')
}

fn run_dir(replica_root: &Path, run_id: &str) -> io::Result<PathBuf> {
    if !valid_run_id(run_id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid trash run id {run_id:?}"),
        ));
    }
    Ok(replica_root.join(TRASH_DIR).join(run_id))
}

/// Adds every file under `dir` to `out`. Anything that vanishes mid-walk (a
/// folder or entry removed by another program) is skipped, never the whole walk.
fn walk_files(dir: &Path, rel: &RelPath, out: &mut Vec<(RelPath, u64)>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for e in entries {
        let e = e?;
        let child = rel.join(&e.file_name().to_string_lossy());
        let md = match fs::symlink_metadata(e.path()) {
            Ok(m) => m,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        if md.is_dir() {
            walk_files(&e.path(), &child, out)?;
        } else {
            out.push((child, md.len()));
        }
    }
    Ok(())
}

/// Every file under `dir` as (relative path, size). Empty only when `dir` itself is missing.
fn list_files(dir: &Path) -> io::Result<Vec<(RelPath, u64)>> {
    match fs::symlink_metadata(dir) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let mut out = Vec::new();
    walk_files(dir, &RelPath::root(), &mut out)?;
    Ok(out)
}

/// Removes `dir` and every folder under it that holds no files, deepest first.
/// Never removes a file; a folder that is not empty is left alone.
fn remove_empty_dirs(dir: &Path) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for e in entries {
        let e = e?;
        if e.file_type()?.is_dir() {
            remove_empty_dirs(&e.path())?;
        }
    }
    match fs::remove_dir(dir) {
        Err(e) if e.kind() != io::ErrorKind::DirectoryNotEmpty => Err(e),
        _ => Ok(()),
    }
}

/// Best-effort tidy-up after a restore: drops the folders the restore emptied,
/// then the manifest and the run folder once nothing else is left in the run.
/// Any error leaves the rest in place.
fn tidy_run(run: &Path) {
    if remove_empty_dirs(&run.join(FILES_DIR)).is_err() {
        return;
    }
    let Ok(entries) = fs::read_dir(run) else {
        return;
    };
    let only_manifest = entries
        .map(|e| e.map(|e| e.file_name() == MANIFEST))
        .collect::<io::Result<Vec<bool>>>()
        .is_ok_and(|names| names.iter().all(|&m| m));
    if only_manifest && fs::remove_file(run.join(MANIFEST)).is_ok() {
        let _ = fs::remove_dir(run);
    }
}

fn read_manifest(run: &Path) -> io::Result<Vec<TrashItem>> {
    let text = match fs::read_to_string(run.join(MANIFEST)) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut items: Vec<TrashItem> = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<ManifestLine>(line) {
            Ok(ManifestLine::Item(item)) => {
                items.retain(|i| i.path != item.path);
                items.push(item);
            }
            Ok(ManifestLine::Restored { restored }) => items.retain(|i| i.path != restored),
            Err(_) => {} // torn last line after a crash
        }
    }
    Ok(items)
}

pub fn run_contents(replica_root: &Path, run_id: &str) -> io::Result<TrashRunContents> {
    let run = run_dir(replica_root, run_id)?;
    let present = list_files(&run.join(FILES_DIR))?;
    let present_paths: HashSet<&RelPath> = present.iter().map(|(p, _)| p).collect();
    let items: Vec<TrashItem> = read_manifest(&run)?
        .into_iter()
        .filter(|i| present_paths.contains(&i.path))
        .collect();
    let recorded: HashSet<&RelPath> = items.iter().map(|i| &i.path).collect();
    let unrecorded = present
        .iter()
        .filter(|(p, _)| !recorded.contains(p))
        .map(|(p, _)| p.clone())
        .collect();
    let bytes = present.iter().map(|(_, s)| s).sum();
    Ok(TrashRunContents {
        items,
        unrecorded,
        bytes,
    })
}

pub fn list_runs(replica_root: &Path) -> io::Result<Vec<TrashRunInfo>> {
    let base = replica_root.join(TRASH_DIR);
    let entries = match fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut runs = Vec::new();
    for e in entries {
        let id = e?.file_name().to_string_lossy().into_owned();
        if !valid_run_id(&id) {
            continue;
        }
        let c = run_contents(replica_root, &id)?;
        runs.push(TrashRunInfo {
            files: c.items.len() + c.unrecorded.len(),
            bytes: c.bytes,
            unrecorded: c.unrecorded.len(),
            id,
        });
    }
    runs.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(runs)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnConflict {
    Refuse,
    /// Move whatever now sits at the original path into a new trash run first.
    TrashExisting,
}

#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    #[error("{0} already exists in the replica")]
    Conflict(RelPath),
    #[error("{0} is not in this trash run")]
    NotInRun(RelPath),
    #[error(transparent)]
    Unsafe(#[from] SafetyError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub fn restore(
    replica_root: &Path,
    run_id: &str,
    paths: &[RelPath],
    on_conflict: OnConflict,
) -> Result<usize, RestoreError> {
    let mut seen = HashSet::new();
    let paths: Vec<RelPath> = paths
        .iter()
        .filter(|p| seen.insert((*p).clone()))
        .cloned()
        .collect();
    let paths = paths.as_slice();
    let run = run_dir(replica_root, run_id)?;
    let contents = run_contents(replica_root, run_id)?;
    let known: HashSet<&RelPath> = contents.items.iter().map(|i| &i.path).collect();
    if let Some(p) = paths.iter().find(|p| !known.contains(p)) {
        return Err(RestoreError::NotInRun(p.clone()));
    }
    let replica = ReplicaRoot::new(replica_root)?;
    let mut targets = Vec::with_capacity(paths.len());
    for p in paths {
        targets.push(replica.target(p)?);
    }
    let conflicts: Vec<usize> = (0..paths.len())
        .filter(|&i| fs::symlink_metadata(&targets[i]).is_ok())
        .collect();
    if let Some(&i) = conflicts
        .iter()
        .find(|&&i| fs::symlink_metadata(&targets[i]).is_ok_and(|m| m.is_dir()))
    {
        return Err(RestoreError::Conflict(paths[i].clone()));
    }
    if let (Some(&i), OnConflict::Refuse) = (conflicts.first(), on_conflict) {
        return Err(RestoreError::Conflict(paths[i].clone()));
    }
    let mut displaced = TrashWriter::new(replica_root, new_run_stamp());
    for &i in &conflicts {
        let md = fs::symlink_metadata(&targets[i])?;
        displaced.move_in(&paths[i], md.len(), modified_ns(&md), TrashReason::Replaced)?;
    }
    let mut manifest = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run.join(MANIFEST))?;
    for (p, target) in paths.iter().zip(&targets) {
        fs::create_dir_all(target.parent().expect("never the root"))?;
        // NOTE: fs::rename replaces a file created after the conflict check above;
        // a no-replace rename is deferred.
        fs::rename(p.to_path(&run.join(FILES_DIR)), target)?;
        let line = serde_json::to_string(&ManifestLine::Restored {
            restored: p.clone(),
        })
        .map_err(io::Error::other)?;
        writeln!(manifest, "{line}")?;
        manifest.sync_data()?;
    }
    drop(manifest);
    tidy_run(&run);
    Ok(paths.len())
}

/// Permanently deletes one trash run. The only permanent delete in replica-sync.
pub fn empty_run(replica_root: &Path, run_id: &str) -> io::Result<()> {
    fs::remove_dir_all(run_dir(replica_root, run_id)?)
}

pub fn runs_older_than(
    replica_root: &Path,
    days: u32,
    now: chrono::NaiveDateTime,
) -> io::Result<Vec<String>> {
    let base = replica_root.join(TRASH_DIR);
    let entries = match fs::read_dir(&base) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let cutoff = now - chrono::Duration::days(i64::from(days));
    let mut old = Vec::new();
    for e in entries {
        let id = e?.file_name().to_string_lossy().into_owned();
        let stamp = id.get(..17).unwrap_or("");
        if valid_run_id(&id)
            && chrono::NaiveDateTime::parse_from_str(stamp, STAMP_FORMAT).is_ok_and(|t| t < cutoff)
        {
            old.push(id);
        }
    }
    old.sort();
    Ok(old)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{T0, ns, read_tree, rel, write_file};
    use std::fs;

    const STAMP: &str = "2026-10-07_120000";

    fn trash_one(root: &Path, p: &str, reason: TrashReason) -> TrashWriter {
        let mut w = TrashWriter::new(root, STAMP.into());
        w.move_in(&rel(p), 3, ns(T0), reason).unwrap();
        w
    }

    #[test]
    fn move_in_creates_run_lazily_and_records_manifest_line() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a/x.txt", b"abc", T0);
        let w = TrashWriter::new(d.path(), STAMP.into());
        assert!(w.run_id().is_none());
        assert!(!d.path().join(TRASH_DIR).exists());
        let w = trash_one(d.path(), "a/x.txt", TrashReason::Deleted);
        assert_eq!(w.run_id(), Some(STAMP));
        assert!(!d.path().join("a/x.txt").exists());
        let run = d.path().join(TRASH_DIR).join(STAMP);
        assert_eq!(
            fs::read(run.join(FILES_DIR).join("a").join("x.txt")).unwrap(),
            b"abc"
        );
        let manifest = fs::read_to_string(run.join(MANIFEST)).unwrap();
        assert_eq!(manifest.lines().count(), 1);
        assert!(manifest.contains("\"reason\":\"deleted\""));
    }

    #[test]
    fn same_stamp_gets_a_suffix() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a", b"1", T0);
        write_file(d.path(), "b", b"1", T0);
        trash_one(d.path(), "a", TrashReason::Deleted);
        let w = trash_one(d.path(), "b", TrashReason::Deleted);
        assert_eq!(w.run_id(), Some("2026-10-07_120000-2"));
    }

    #[test]
    fn list_and_contents_show_items_and_unrecorded_files() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Replaced);
        // a file that got into the run without a manifest line (crash between rename and append)
        write_file(
            &d.path().join(TRASH_DIR).join(STAMP).join(FILES_DIR),
            "lost.bin",
            b"12345",
            T0,
        );
        let runs = list_runs(d.path()).unwrap();
        assert_eq!(
            runs,
            vec![TrashRunInfo {
                id: STAMP.into(),
                files: 2,
                bytes: 8,
                unrecorded: 1
            }]
        );
        let c = run_contents(d.path(), STAMP).unwrap();
        assert_eq!(
            c.items,
            vec![TrashItem {
                path: rel("x.txt"),
                size: 3,
                mtime_ns: ns(T0),
                reason: TrashReason::Replaced
            }]
        );
        assert_eq!(c.unrecorded, vec![rel("lost.bin")]);
    }

    #[test]
    fn torn_last_manifest_line_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        let m = d.path().join(TRASH_DIR).join(STAMP).join(MANIFEST);
        let mut text = fs::read_to_string(&m).unwrap();
        text.push_str("{\"path\":\"y.t");
        fs::write(&m, text).unwrap();
        assert_eq!(run_contents(d.path(), STAMP).unwrap().items.len(), 1);
    }

    #[test]
    fn restore_puts_files_back_and_removes_an_emptied_run() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a/x.txt", b"abc", T0);
        trash_one(d.path(), "a/x.txt", TrashReason::Deleted);
        assert_eq!(
            restore(d.path(), STAMP, &[rel("a/x.txt")], OnConflict::Refuse).unwrap(),
            1
        );
        assert_eq!(read_tree(d.path()).get("a/x.txt").unwrap(), b"abc");
        assert!(!d.path().join(TRASH_DIR).join(STAMP).exists());
    }

    #[test]
    fn restore_conflict_refuses_or_trashes_the_existing_file() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"old", T0);
        trash_one(d.path(), "x.txt", TrashReason::Replaced);
        write_file(d.path(), "x.txt", b"new", T0 + 9);
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("x.txt")], OnConflict::Refuse),
            Err(RestoreError::Conflict(_))
        ));
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"new");
        restore(d.path(), STAMP, &[rel("x.txt")], OnConflict::TrashExisting).unwrap();
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"old");
        let runs = list_runs(d.path()).unwrap();
        assert_eq!(runs.len(), 1, "the displaced 'new' file is in a fresh run");
        assert_ne!(runs[0].id, STAMP);
    }

    #[test]
    fn restore_rejects_paths_not_in_the_run() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("other.txt")], OnConflict::Refuse),
            Err(RestoreError::NotInRun(_))
        ));
    }

    #[test]
    fn empty_run_deletes_permanently_and_rejects_bad_ids() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        assert!(empty_run(d.path(), "../..").is_err());
        empty_run(d.path(), STAMP).unwrap();
        assert!(list_runs(d.path()).unwrap().is_empty());
    }

    #[test]
    fn runs_older_than_compares_the_stamp() {
        let d = tempfile::tempdir().unwrap();
        for id in ["2026-08-01_090000", "2026-10-06_090000"] {
            fs::create_dir_all(d.path().join(TRASH_DIR).join(id)).unwrap();
        }
        let now =
            chrono::NaiveDateTime::parse_from_str("2026-10-07_120000", "%Y-%m-%d_%H%M%S").unwrap();
        assert_eq!(
            runs_older_than(d.path(), 30, now).unwrap(),
            vec!["2026-08-01_090000".to_string()]
        );
    }

    #[test]
    fn trashing_the_same_path_twice_in_one_run_keeps_the_first_copy() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"first", T0);
        let mut w = trash_one(d.path(), "x.txt", TrashReason::Deleted);
        write_file(d.path(), "x.txt", b"second", T0);
        let err = w
            .move_in(&rel("x.txt"), 6, ns(T0), TrashReason::Deleted)
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        let run = d.path().join(TRASH_DIR).join(STAMP).join(FILES_DIR);
        assert_eq!(fs::read(run.join("x.txt")).unwrap(), b"first");
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"second");
    }

    #[test]
    fn restore_with_duplicate_paths_restores_once() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        let n = restore(
            d.path(),
            STAMP,
            &[rel("x.txt"), rel("x.txt")],
            OnConflict::Refuse,
        )
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"abc");
    }

    #[test]
    fn restore_refuses_a_folder_in_the_way() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        write_file(d.path(), "x.txt/inner", b"i", T0);
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("x.txt")], OnConflict::TrashExisting),
            Err(RestoreError::Conflict(_))
        ));
        assert!(d.path().join("x.txt/inner").exists());
        assert_eq!(run_contents(d.path(), STAMP).unwrap().items.len(), 1);
    }
    #[test]
    fn an_entry_that_vanished_mid_walk_is_skipped_not_the_whole_run() {
        let d = tempfile::tempdir().unwrap();
        let mut out = vec![(rel("kept.txt"), 3)];
        walk_files(&d.path().join("gone"), &rel("gone"), &mut out).unwrap();
        assert_eq!(out, vec![(rel("kept.txt"), 3)]);
        assert!(list_files(&d.path().join("missing")).unwrap().is_empty());
    }

    #[test]
    fn restoring_one_of_two_keeps_the_other_and_the_run() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a/x.txt", b"abc", T0);
        write_file(d.path(), "b/y.txt", b"def", T0);
        let mut w = trash_one(d.path(), "a/x.txt", TrashReason::Deleted);
        w.move_in(&rel("b/y.txt"), 3, ns(T0), TrashReason::Deleted)
            .unwrap();
        restore(d.path(), STAMP, &[rel("a/x.txt")], OnConflict::Refuse).unwrap();
        let run = d.path().join(TRASH_DIR).join(STAMP);
        assert_eq!(
            fs::read(run.join(FILES_DIR).join("b").join("y.txt")).unwrap(),
            b"def"
        );
        assert!(run.join(MANIFEST).exists());
        assert!(
            !run.join(FILES_DIR).join("a").exists(),
            "emptied folder tidied"
        );
        let c = run_contents(d.path(), STAMP).unwrap();
        assert_eq!(
            c.items.iter().map(|i| i.path.as_str()).collect::<Vec<_>>(),
            vec!["b/y.txt"]
        );
    }

    #[test]
    fn restore_keeps_a_run_holding_anything_besides_the_manifest() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        let run = d.path().join(TRASH_DIR).join(STAMP);
        fs::write(run.join("notes.txt"), b"keep me").unwrap();
        restore(d.path(), STAMP, &[rel("x.txt")], OnConflict::Refuse).unwrap();
        assert_eq!(fs::read(run.join("notes.txt")).unwrap(), b"keep me");
        assert!(run.join(MANIFEST).exists());
    }
}
