//! Parallel walk of one side of a pair into a sorted `Snapshot`.

use crate::model::{Entry, Kind, Problem, ProblemKind, RelPath, Snapshot, modified_ns};
use crate::rules::{SkipRules, TEMP_SUFFIX};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Live progress of a scan, polled by the caller (e.g. a UI timer).
#[derive(Debug, Default)]
pub struct ScanCounters {
    files: AtomicU64,
    bytes: AtomicU64,
    current: Mutex<String>,
    cancelled: AtomicBool,
}

impl ScanCounters {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Files and bytes found so far, and the folder being read.
    pub fn read(&self) -> (u64, u64, String) {
        (
            self.files.load(Ordering::Relaxed),
            self.bytes.load(Ordering::Relaxed),
            self.current.lock().unwrap().clone(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error("scan cancelled")]
    Cancelled,
    #[error("cannot read {}: {source}", path.display())]
    Root { path: PathBuf, source: io::Error },
}

#[derive(Default)]
struct Collected {
    entries: Mutex<Vec<Entry>>,
    problems: Mutex<Vec<Problem>>,
    leftovers: Mutex<Vec<RelPath>>,
    unlisted: Mutex<Vec<RelPath>>,
}

pub fn scan(
    root: &Path,
    rules: &SkipRules,
    counters: &ScanCounters,
) -> Result<Snapshot, ScanError> {
    let root_error = |source| ScanError::Root {
        path: root.to_path_buf(),
        source,
    };
    if !fs::metadata(root).map_err(root_error)?.is_dir() {
        return Err(root_error(io::Error::other("not a folder")));
    }
    let out = Collected::default();
    rayon::scope(|s| walk(s, root, RelPath::root(), rules, counters, &out));
    if counters.is_cancelled() {
        return Err(ScanError::Cancelled);
    }
    let mut entries = out.entries.into_inner().unwrap();
    entries.sort_unstable_by(|a, b| a.rel.cmp(&b.rel));
    Ok(Snapshot {
        root: root.to_path_buf(),
        entries,
        problems: out.problems.into_inner().unwrap(),
        leftovers: out.leftovers.into_inner().unwrap(),
        unlisted: out.unlisted.into_inner().unwrap(),
    })
}

fn walk<'s>(
    s: &rayon::Scope<'s>,
    root: &'s Path,
    dir: RelPath,
    rules: &'s SkipRules,
    counters: &'s ScanCounters,
    out: &'s Collected,
) {
    if counters.is_cancelled() {
        return;
    }
    *counters.current.lock().unwrap() = dir.as_str().to_owned();
    let read = match fs::read_dir(dir.to_path(root)) {
        Ok(r) => r,
        Err(e) => {
            out.problems.lock().unwrap().push(Problem {
                rel: dir,
                kind: ProblemKind::Unreadable,
                detail: e.to_string(),
            });
            return;
        }
    };
    let (mut entries, mut problems) = (Vec::new(), Vec::new());
    let (mut leftovers, mut unlisted) = (Vec::new(), Vec::new());
    for item in read {
        let entry = match item {
            Ok(e) => e,
            Err(e) => {
                // A listing that fails part-way: block the whole folder.
                problems.push(Problem {
                    rel: dir.clone(),
                    kind: ProblemKind::Unreadable,
                    detail: e.to_string(),
                });
                continue;
            }
        };
        let raw = entry.file_name();
        let rel = dir.join(&raw.to_string_lossy());
        if raw.to_str().is_none() {
            problems.push(Problem {
                rel,
                kind: ProblemKind::Unreadable,
                detail: "name is not valid Unicode".into(),
            });
            continue;
        }
        if rel.name().ends_with(TEMP_SUFFIX) {
            unlisted.push(rel.clone());
            leftovers.push(rel);
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                problems.push(Problem {
                    rel,
                    kind: ProblemKind::Unreadable,
                    detail: e.to_string(),
                });
                continue;
            }
        };
        if rules.is_skipped(&rel, file_type.is_dir()) {
            unlisted.push(rel);
            continue;
        }
        if file_type.is_symlink() {
            entries.push(Entry {
                rel,
                kind: Kind::Link,
                size: 0,
                mtime_ns: 0,
            });
            continue;
        }
        let md = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                problems.push(Problem {
                    rel,
                    kind: ProblemKind::Unreadable,
                    detail: e.to_string(),
                });
                continue;
            }
        };
        if md.is_dir() {
            entries.push(Entry {
                rel: rel.clone(),
                kind: Kind::Dir,
                size: 0,
                mtime_ns: modified_ns(&md),
            });
            s.spawn(move |s| walk(s, root, rel, rules, counters, out));
        } else if !md.is_file() {
            // FIFO, socket or device: never copied, never deleted.
            problems.push(Problem {
                rel,
                kind: ProblemKind::NotRegularFile,
                detail: String::new(),
            });
        } else {
            counters.files.fetch_add(1, Ordering::Relaxed);
            counters.bytes.fetch_add(md.len(), Ordering::Relaxed);
            entries.push(Entry {
                rel,
                kind: Kind::File,
                size: md.len(),
                mtime_ns: modified_ns(&md),
            });
        }
    }
    out.entries.lock().unwrap().extend(entries);
    out.problems.lock().unwrap().extend(problems);
    out.leftovers.lock().unwrap().extend(leftovers);
    out.unlisted.lock().unwrap().extend(unlisted);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Kind;
    use crate::testutil::{T0, ns, write_file};

    fn no_rules() -> SkipRules {
        SkipRules::new(&[]).unwrap()
    }

    #[test]
    fn lists_files_and_folders_sorted_with_size_and_mtime() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "b.txt", b"hello", T0);
        write_file(d.path(), "a/c.txt", b"xy", T0 + 5);
        let c = ScanCounters::default();
        let s = scan(d.path(), &no_rules(), &c).unwrap();
        let got: Vec<_> = s
            .entries
            .iter()
            .map(|e| (e.rel.as_str(), e.kind, e.size))
            .collect();
        assert_eq!(
            got,
            vec![
                ("a", Kind::Dir, 0),
                ("a/c.txt", Kind::File, 2),
                ("b.txt", Kind::File, 5)
            ]
        );
        assert_eq!(s.entries[1].mtime_ns, ns(T0 + 5));
        assert_eq!(c.read().0, 2);
        assert_eq!(c.read().1, 7);
    }

    #[test]
    fn skip_rules_prune_folders_and_files() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "Temp/x.txt", b"1", T0);
        write_file(d.path(), "Thumbs.db", b"1", T0);
        write_file(d.path(), "keep.txt", b"1", T0);
        let rules = SkipRules::new(&["Temp/".to_string()]).unwrap();
        let s = scan(d.path(), &rules, &ScanCounters::default()).unwrap();
        let names: Vec<_> = s.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(names, vec!["keep.txt"]);
        let mut unlisted: Vec<_> = s.unlisted.iter().map(|r| r.as_str()).collect();
        unlisted.sort();
        assert_eq!(unlisted, vec!["Temp", "Thumbs.db"]);
    }

    #[test]
    fn temp_files_are_leftovers_not_entries() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a/x.jpg.replica-sync.tmp", b"partial", T0);
        let s = scan(d.path(), &no_rules(), &ScanCounters::default()).unwrap();
        assert_eq!(
            s.leftovers.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
            vec!["a/x.jpg.replica-sync.tmp"]
        );
        assert_eq!(s.unlisted, s.leftovers);
        assert!(
            s.entries
                .iter()
                .all(|e| !e.rel.as_str().ends_with(TEMP_SUFFIX))
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_links_and_not_followed() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "real/a.txt", b"1", T0);
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("alias")).unwrap();
        let s = scan(d.path(), &no_rules(), &ScanCounters::default()).unwrap();
        let alias: Vec<_> = s
            .entries
            .iter()
            .filter(|e| e.rel.as_str().starts_with("alias"))
            .collect();
        assert_eq!(alias.len(), 1);
        assert_eq!(alias[0].kind, Kind::Link);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_folder_is_a_problem_and_scan_continues() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root can read anything
        }
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "locked/a.txt", b"1", T0);
        write_file(d.path(), "ok.txt", b"1", T0);
        let locked = d.path().join("locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let s = scan(d.path(), &no_rules(), &ScanCounters::default());
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let s = s.unwrap();
        assert_eq!(s.problems.len(), 1);
        assert_eq!(s.problems[0].rel.as_str(), "locked");
        assert!(s.entries.iter().any(|e| e.rel.as_str() == "ok.txt"));
    }

    #[test]
    fn cancelled_scan_returns_cancelled() {
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "a.txt", b"1", T0);
        let c = ScanCounters::default();
        c.cancel();
        assert!(matches!(
            scan(d.path(), &no_rules(), &c),
            Err(ScanError::Cancelled)
        ));
    }

    #[test]
    fn missing_root_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let err = scan(
            &d.path().join("nope"),
            &no_rules(),
            &ScanCounters::default(),
        )
        .unwrap_err();
        assert!(matches!(err, ScanError::Root { .. }));
    }
    #[cfg(unix)]
    #[test]
    fn fifo_is_a_problem_not_a_file() {
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        write_file(d.path(), "ok.txt", b"1", T0);
        let fifo = d.path().join("pipe");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o644) }, 0);
        let s = scan(d.path(), &no_rules(), &ScanCounters::default()).unwrap();
        let names: Vec<_> = s.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(names, vec!["ok.txt"]);
        assert_eq!(
            s.problems,
            vec![Problem {
                rel: RelPath::new("pipe").unwrap(),
                kind: ProblemKind::NotRegularFile,
                detail: String::new()
            }]
        );
    }
}
