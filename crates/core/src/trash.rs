//! `.sync-trash`: where every deleted or replaced replica file goes, recorded
//! one manifest line at a time so a crash never loses track of a file.

use crate::model::{RelPath, modified_ns};
use crate::safety::{ReplicaRoot, SafetyError};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// `<replica>/.sync-trash`, refused when it is a link (or junction): following
/// one would trash, restore and permanently delete files outside the replica.
fn trash_base(replica_root: &Path) -> io::Result<PathBuf> {
    let base = replica_root.join(TRASH_DIR);
    match fs::symlink_metadata(&base) {
        Ok(m) if m.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the .sync-trash folder is a link",
        )),
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(base),
    }
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
        let line = serde_json::to_string(&ManifestLine::Item(TrashItem {
            path: rel.clone(),
            size,
            mtime_ns,
            reason,
        }))
        .map_err(io::Error::other)?;
        let recorded = manifest.metadata()?.len();
        let from = rel.to_path(&self.replica_root);
        fs::rename(&from, &dest)?;
        if let Err(e) = append_line(manifest, &line) {
            // Undo, so a full or failing disk never leaves a trashed file the
            // manifest does not list (or a torn line that hides later ones).
            if manifest.set_len(recorded).is_err() {
                // (Windows append handles cannot truncate.) End the torn line so
                // the next one still parses; replay skips the torn one.
                let _ = manifest.write_all(b"\n");
            }
            let _ = fs::rename(&dest, &from);
            return Err(e);
        }
        Ok(())
    }

    fn open_run(&self) -> io::Result<(String, PathBuf, File)> {
        let base = trash_base(&self.replica_root)?;
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

#[cfg(test)]
thread_local! {
    /// Appending a manifest line fails with this errno (tests).
    pub(crate) static MANIFEST_ERRNO: std::cell::Cell<Option<i32>> =
        const { std::cell::Cell::new(None) };
}

fn append_line(manifest: &mut File, line: &str) -> io::Result<()> {
    #[cfg(test)]
    if let Some(errno) = MANIFEST_ERRNO.get() {
        // Like a disk that fills up mid-line: part of it lands, then the error.
        let _ = manifest.write_all(&line.as_bytes()[..line.len() / 2]);
        return Err(io::Error::from_raw_os_error(errno));
    }
    writeln!(manifest, "{line}")?;
    manifest.sync_data()
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
    Ok(trash_base(replica_root)?.join(run_id))
}

/// A trash run's folder, its `files` folder and its manifest.
struct RunPaths {
    run: PathBuf,
    files: PathBuf,
    manifest: PathBuf,
}

/// `run_dir` plus its `files` and manifest, refused when any of the three is a
/// link (or junction): following one would read, move into the replica,
/// append to or delete files outside it. A missing `files` or manifest is fine.
fn run_paths(replica_root: &Path, run_id: &str) -> io::Result<RunPaths> {
    let run = run_dir(replica_root, run_id)?;
    let paths = RunPaths {
        files: run.join(FILES_DIR),
        manifest: run.join(MANIFEST),
        run,
    };
    for p in [&paths.run, &paths.files, &paths.manifest] {
        match fs::symlink_metadata(p) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("trash run {run_id} is a link"),
                ));
            }
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
    }
    Ok(paths)
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
    // Replay in one pass: the latest line for a path wins, and its position in
    // the manifest decides the order (as re-appending it to a list would).
    let mut live: HashMap<RelPath, (usize, TrashItem)> = HashMap::new();
    for (n, line) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        match serde_json::from_str::<ManifestLine>(line) {
            Ok(ManifestLine::Item(item)) => {
                live.insert(item.path.clone(), (n, item));
            }
            Ok(ManifestLine::Restored { restored }) => {
                live.remove(&restored);
            }
            Err(_) => {} // torn last line after a crash
        }
    }
    let mut items: Vec<(usize, TrashItem)> = live.into_values().collect();
    items.sort_unstable_by_key(|(n, _)| *n);
    Ok(items.into_iter().map(|(_, item)| item).collect())
}

pub fn run_contents(replica_root: &Path, run_id: &str) -> io::Result<TrashRunContents> {
    let RunPaths { run, files, .. } = run_paths(replica_root, run_id)?;
    let present = list_files(&files)?;
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
    let base = trash_base(replica_root)?;
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
        // Counts come from the manifest alone, so the list costs one small read
        // per run instead of a walk of every trashed file. Files that got into
        // a run without a manifest line show up in `run_contents` (`unrecorded`).
        let items = read_manifest(&run_paths(replica_root, &id)?.run)?;
        runs.push(TrashRunInfo {
            files: items.len(),
            bytes: items.iter().map(|i| i.size).sum(),
            unrecorded: 0,
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
    let RunPaths {
        run,
        files,
        manifest: manifest_path,
    } = run_paths(replica_root, run_id)?;
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
        .open(&manifest_path)?;
    // Defence in depth: every source must still resolve inside this run's own
    // `files` folder (a link planted below it would otherwise be followed).
    let files_root = fs::canonicalize(&files)?;
    for (p, target) in paths.iter().zip(&targets) {
        let source = p.to_path(&files);
        let parent = fs::canonicalize(source.parent().expect("never the root"))?;
        if !parent.starts_with(&files_root) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("trash run {run_id}: {p} resolves outside the run, which is a link"),
            )
            .into());
        }
        fs::create_dir_all(target.parent().expect("never the root"))?;
        // NOTE: fs::rename replaces a file created after the conflict check above;
        // a no-replace rename is deferred.
        fs::rename(&source, target)?;
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
    fs::remove_dir_all(run_paths(replica_root, run_id)?.run)
}

pub fn runs_older_than(
    replica_root: &Path,
    days: u32,
    now: chrono::NaiveDateTime,
) -> io::Result<Vec<String>> {
    let base = trash_base(replica_root)?;
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
    fn a_failed_manifest_write_puts_the_file_back_and_keeps_the_manifest_whole() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a/first.txt", b"one", T0);
        write_file(d.path(), "a/x.txt", b"abc", T0);
        let mut w = trash_one(d.path(), "a/first.txt", TrashReason::Deleted);
        let run = d.path().join(TRASH_DIR).join(STAMP);
        let manifest_before = fs::read(run.join(MANIFEST)).unwrap();
        MANIFEST_ERRNO.set(Some(28)); // ENOSPC
        let err = w.move_in(&rel("a/x.txt"), 3, ns(T0), TrashReason::Replaced);
        MANIFEST_ERRNO.set(None);
        assert_eq!(err.unwrap_err().raw_os_error(), Some(28));
        assert_eq!(fs::read(d.path().join("a/x.txt")).unwrap(), b"abc");
        assert!(!run.join(FILES_DIR).join("a").join("x.txt").exists());
        #[cfg(unix)]
        assert_eq!(fs::read(run.join(MANIFEST)).unwrap(), manifest_before);
        let _ = manifest_before;
        let listed = |w: &TrashWriter| -> Vec<RelPath> {
            run_contents(d.path(), w.run_id().unwrap())
                .unwrap()
                .items
                .into_iter()
                .map(|i| i.path)
                .collect()
        };
        assert_eq!(listed(&w), vec![rel("a/first.txt")]);
        // The run is still usable and lists exactly what it holds.
        w.move_in(&rel("a/x.txt"), 3, ns(T0), TrashReason::Replaced)
            .unwrap();
        assert_eq!(listed(&w), vec![rel("a/first.txt"), rel("a/x.txt")]);
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
    fn manifest_replay_is_linear_and_keeps_manifest_order() {
        let d = tempfile::tempdir().unwrap();
        let run = d.path().join("run");
        fs::create_dir(&run).unwrap();
        let mut text = String::new();
        for i in 0..50_000 {
            let item = ManifestLine::Item(TrashItem {
                path: rel(&format!("d/f{i}.bin")),
                size: 1,
                mtime_ns: 0,
                reason: TrashReason::Deleted,
            });
            text.push_str(&serde_json::to_string(&item).unwrap());
            text.push('\n');
        }
        for i in (0..50_000).step_by(5) {
            let line = ManifestLine::Restored {
                restored: rel(&format!("d/f{i}.bin")),
            };
            text.push_str(&serde_json::to_string(&line).unwrap());
            text.push('\n');
        }
        fs::write(run.join(MANIFEST), text).unwrap();
        let started = std::time::Instant::now();
        let items = read_manifest(&run).unwrap();
        let elapsed = started.elapsed();
        assert_eq!(items.len(), 40_000);
        assert_eq!(items[0].path, rel("d/f1.bin"));
        assert_eq!(items[3].path, rel("d/f4.bin"));
        assert_eq!(items[4].path, rel("d/f6.bin"));
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "replay took {elapsed:?}"
        );
    }

    #[test]
    fn a_re_trashed_path_moves_to_its_latest_position() {
        let d = tempfile::tempdir().unwrap();
        let run = d.path().join("run");
        fs::create_dir(&run).unwrap();
        let item = |p: &str, size| {
            serde_json::to_string(&ManifestLine::Item(TrashItem {
                path: rel(p),
                size,
                mtime_ns: 0,
                reason: TrashReason::Deleted,
            }))
            .unwrap()
        };
        fs::write(
            run.join(MANIFEST),
            [item("a", 1), item("b", 2), item("a", 3)].join("\n"),
        )
        .unwrap();
        let items = read_manifest(&run).unwrap();
        let got: Vec<_> = items.iter().map(|i| (i.path.as_str(), i.size)).collect();
        assert_eq!(got, vec![("b", 2), ("a", 3)]);
    }

    #[test]
    fn list_runs_counts_the_manifest_without_walking_files() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..3 {
            write_file(d.path(), &format!("x{i}.txt"), b"abc", T0);
        }
        let mut w = TrashWriter::new(d.path(), STAMP.into());
        for i in 0..3 {
            w.move_in(&rel(&format!("x{i}.txt")), 3, ns(T0), TrashReason::Deleted)
                .unwrap();
        }
        drop(w);
        // With the trashed files gone, the counts can only have come from the manifest.
        fs::remove_dir_all(d.path().join(TRASH_DIR).join(STAMP).join(FILES_DIR)).unwrap();
        let runs = list_runs(d.path()).unwrap();
        assert_eq!((runs[0].files, runs[0].bytes), (3, 9));
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
        // The list counts the manifest only; the unrecorded file shows in the contents.
        let runs = list_runs(d.path()).unwrap();
        assert_eq!(
            runs,
            vec![TrashRunInfo {
                id: STAMP.into(),
                files: 1,
                bytes: 3,
                unrecorded: 0
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
    fn is_run_link_err(e: &io::Error) -> bool {
        e.kind() == io::ErrorKind::InvalidInput && e.to_string().ends_with("is a link")
    }

    /// A real trash run holding `secret.txt`, in a replica outside the one under test.
    fn outside_run() -> tempfile::TempDir {
        let o = tempfile::tempdir().unwrap();
        write_file(o.path(), "secret.txt", b"abc", T0);
        trash_one(o.path(), "secret.txt", TrashReason::Deleted);
        fs::create_dir(
            o.path()
                .join(TRASH_DIR)
                .join(STAMP)
                .join(FILES_DIR)
                .join("emptydir"),
        )
        .unwrap();
        o
    }

    fn outside_secret(o: &Path) -> PathBuf {
        o.join(TRASH_DIR)
            .join(STAMP)
            .join(FILES_DIR)
            .join("secret.txt")
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_trash_run_is_refused() {
        let o = outside_run();
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join(TRASH_DIR)).unwrap();
        std::os::unix::fs::symlink(
            o.path().join(TRASH_DIR).join(STAMP),
            d.path().join(TRASH_DIR).join(STAMP),
        )
        .unwrap();
        assert!(is_run_link_err(&run_contents(d.path(), STAMP).unwrap_err()));
        assert!(is_run_link_err(&list_runs(d.path()).unwrap_err()));
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("secret.txt")], OnConflict::Refuse),
            Err(RestoreError::Io(e)) if is_run_link_err(&e)
        ));
        assert!(is_run_link_err(&empty_run(d.path(), STAMP).unwrap_err()));
        assert_eq!(fs::read(outside_secret(o.path())).unwrap(), b"abc");
        assert!(
            o.path()
                .join(TRASH_DIR)
                .join(STAMP)
                .join(FILES_DIR)
                .join("emptydir")
                .is_dir()
        );
        assert!(!d.path().join("secret.txt").exists());
    }

    /// The run folder is real but its `files` folder links outside, with a
    /// manifest line for a file there: restore must not pull it into the replica.
    fn replica_with_linked_files(o: &Path, link: impl Fn(&Path, &Path)) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let run = d.path().join(TRASH_DIR).join(STAMP);
        fs::create_dir_all(&run).unwrap();
        fs::copy(
            o.join(TRASH_DIR).join(STAMP).join(MANIFEST),
            run.join(MANIFEST),
        )
        .unwrap();
        link(
            &run.join(FILES_DIR),
            &o.join(TRASH_DIR).join(STAMP).join(FILES_DIR),
        );
        d
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_run_files_folder_is_refused() {
        let o = outside_run();
        let d = replica_with_linked_files(o.path(), |link, target| {
            std::os::unix::fs::symlink(target, link).unwrap()
        });
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("secret.txt")], OnConflict::Refuse),
            Err(RestoreError::Io(e)) if is_run_link_err(&e)
        ));
        assert!(is_run_link_err(&run_contents(d.path(), STAMP).unwrap_err()));
        assert!(is_run_link_err(&list_runs(d.path()).unwrap_err()));
        assert_eq!(fs::read(outside_secret(o.path())).unwrap(), b"abc");
        assert!(
            o.path()
                .join(TRASH_DIR)
                .join(STAMP)
                .join(FILES_DIR)
                .join("emptydir")
                .is_dir()
        );
        assert!(!d.path().join("secret.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn a_junctioned_run_files_folder_is_refused() {
        let o = outside_run();
        let d = replica_with_linked_files(o.path(), |link, target| {
            crate::testutil::junction(link, target)
        });
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("secret.txt")], OnConflict::Refuse),
            Err(RestoreError::Io(e)) if is_run_link_err(&e)
        ));
        assert_eq!(fs::read(outside_secret(o.path())).unwrap(), b"abc");
        assert!(!d.path().join("secret.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_run_manifest_is_refused() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        let run = d.path().join(TRASH_DIR).join(STAMP);
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("victim.jsonl");
        fs::copy(run.join(MANIFEST), &victim).unwrap();
        let before = fs::read(&victim).unwrap();
        fs::remove_file(run.join(MANIFEST)).unwrap();
        std::os::unix::fs::symlink(&victim, run.join(MANIFEST)).unwrap();
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("x.txt")], OnConflict::Refuse),
            Err(RestoreError::Io(e)) if is_run_link_err(&e)
        ));
        assert_eq!(fs::read(&victim).unwrap(), before);
        assert!(!d.path().join("x.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_sync_trash_folder_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_file(
            outside.path(),
            &format!("{STAMP}/{FILES_DIR}/kept.txt"),
            b"k",
            T0,
        );
        std::os::unix::fs::symlink(outside.path(), d.path().join(TRASH_DIR)).unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        let is_link_err = |e: &io::Error| {
            e.kind() == io::ErrorKind::InvalidInput
                && e.to_string() == "the .sync-trash folder is a link"
        };
        let is_link_error = |e: io::Error| is_link_err(&e);
        let mut w = TrashWriter::new(d.path(), "2026-10-07_130000".into());
        assert!(is_link_error(
            w.move_in(&rel("x.txt"), 3, ns(T0), TrashReason::Deleted)
                .unwrap_err()
        ));
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"abc");
        assert!(is_link_error(empty_run(d.path(), STAMP).unwrap_err()));
        assert!(is_link_error(list_runs(d.path()).unwrap_err()));
        assert!(is_link_error(run_contents(d.path(), STAMP).unwrap_err()));
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("kept.txt")], OnConflict::Refuse),
            Err(RestoreError::Io(e)) if is_link_err(&e)
        ));
        let now = chrono::NaiveDateTime::parse_from_str("2027-01-01_000000", STAMP_FORMAT).unwrap();
        assert!(is_link_error(
            runs_older_than(d.path(), 30, now).unwrap_err()
        ));
        assert_eq!(
            fs::read(outside.path().join(STAMP).join(FILES_DIR).join("kept.txt")).unwrap(),
            b"k"
        );
        assert!(!outside.path().join("2026-10-07_130000").exists());
    }
    #[cfg(windows)]
    #[test]
    fn a_junctioned_sync_trash_folder_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write_file(
            outside.path(),
            &format!("{STAMP}/{FILES_DIR}/kept.txt"),
            b"k",
            T0,
        );
        crate::testutil::junction(&d.path().join(TRASH_DIR), outside.path());
        write_file(d.path(), "x.txt", b"abc", T0);
        let is_link_err = |e: &io::Error| {
            e.kind() == io::ErrorKind::InvalidInput
                && e.to_string() == "the .sync-trash folder is a link"
        };
        let is_link_error = |e: io::Error| is_link_err(&e);
        let mut w = TrashWriter::new(d.path(), "2026-10-07_130000".into());
        assert!(is_link_error(
            w.move_in(&rel("x.txt"), 3, ns(T0), TrashReason::Deleted)
                .unwrap_err()
        ));
        assert_eq!(fs::read(d.path().join("x.txt")).unwrap(), b"abc");
        assert!(is_link_error(empty_run(d.path(), STAMP).unwrap_err()));
        assert!(is_link_error(list_runs(d.path()).unwrap_err()));
        assert!(is_link_error(run_contents(d.path(), STAMP).unwrap_err()));
        assert!(matches!(
            restore(d.path(), STAMP, &[rel("kept.txt")], OnConflict::Refuse),
            Err(RestoreError::Io(e)) if is_link_err(&e)
        ));
        let now = chrono::NaiveDateTime::parse_from_str("2027-01-01_000000", STAMP_FORMAT).unwrap();
        assert!(is_link_error(
            runs_older_than(d.path(), 30, now).unwrap_err()
        ));
        assert_eq!(
            fs::read(outside.path().join(STAMP).join(FILES_DIR).join("kept.txt")).unwrap(),
            b"k"
        );
        assert!(!outside.path().join("2026-10-07_130000").exists());
    }

    #[cfg(windows)]
    #[test]
    fn the_trash_folder_is_hidden() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_HIDDEN, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
        };
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "x.txt", b"abc", T0);
        trash_one(d.path(), "x.txt", TrashReason::Deleted);
        let name: Vec<u16> = d
            .path()
            .join(TRASH_DIR)
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let attrs = unsafe { GetFileAttributesW(name.as_ptr()) };
        assert_ne!(attrs, INVALID_FILE_ATTRIBUTES);
        assert_ne!(attrs & FILE_ATTRIBUTE_HIDDEN, 0, "attributes {attrs:#x}");
    }
}
