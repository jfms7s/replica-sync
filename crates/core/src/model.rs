//! Types shared by every stage: relative paths, scanned entries, changes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Modified times this close count as equal (exFAT/FAT32 store 2-second steps).
pub const MTIME_TOLERANCE_NS: i64 = 2_000_000_000;

/// A path relative to a pair's folder, `/`-separated. The empty path is the folder itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid relative path {0:?}")]
pub struct InvalidPath(pub String);

impl RelPath {
    pub fn root() -> RelPath {
        RelPath(String::new())
    }

    /// Validates a non-empty relative path: no empty, `.` or `..` components
    /// (and on Windows no `\` or `:`, which would let a component escape).
    pub fn new(s: &str) -> Result<RelPath, InvalidPath> {
        let bad_component = |c: &str| {
            c.is_empty()
                || c == "."
                || c == ".."
                || (cfg!(windows) && (c.contains('\\') || c.contains(':')))
        };
        if s.split('/').any(bad_component) {
            return Err(InvalidPath(s.to_owned()));
        }
        Ok(RelPath(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Appends `name`, which may itself contain `/`-separated components.
    pub fn join(&self, name: &str) -> RelPath {
        if self.0.is_empty() {
            RelPath(name.to_owned())
        } else {
            RelPath(format!("{}/{}", self.0, name))
        }
    }

    pub fn name(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or("")
    }

    pub fn parent(&self) -> Option<RelPath> {
        if self.0.is_empty() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => RelPath(self.0[..i].to_owned()),
            None => RelPath::root(),
        })
    }

    pub fn depth(&self) -> usize {
        if self.0.is_empty() {
            0
        } else {
            self.0.matches('/').count() + 1
        }
    }

    /// True for `dir` itself and anything below it.
    pub fn is_within(&self, dir: &RelPath) -> bool {
        self.strip_dir(dir).is_some() || self == dir
    }

    /// The part of `self` below `dir`, if `self` is strictly inside it.
    pub fn strip_dir(&self, dir: &RelPath) -> Option<&str> {
        if dir.is_root() {
            return (!self.is_root()).then_some(self.0.as_str());
        }
        self.0.strip_prefix(dir.0.as_str())?.strip_prefix('/')
    }

    pub fn fold(&self) -> String {
        fold(&self.0)
    }

    pub fn to_path(&self, root: &Path) -> PathBuf {
        let mut p = root.to_path_buf();
        for c in self.0.split('/').filter(|c| !c.is_empty()) {
            p.push(c);
        }
        p
    }
}

/// Case folding used to compare names on case-insensitive drives.
pub fn fold(s: &str) -> String {
    s.to_lowercase()
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<RelPath> for String {
    fn from(p: RelPath) -> String {
        p.0
    }
}

impl TryFrom<String> for RelPath {
    type Error = InvalidPath;
    fn try_from(s: String) -> Result<RelPath, InvalidPath> {
        if s.is_empty() {
            Ok(RelPath::root())
        } else {
            RelPath::new(&s)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Kind {
    File,
    Dir,
    /// Symlink or junction: reported, never followed.
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub rel: RelPath,
    pub kind: Kind,
    pub size: u64,
    pub mtime_ns: i64,
}

/// Something the scan could not read; nothing under it may change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub rel: RelPath,
    pub reason: String,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub root: PathBuf,
    /// Sorted by `rel`.
    pub entries: Vec<Entry>,
    pub problems: Vec<Problem>,
    /// `*.replica-sync.tmp` files left by an interrupted run.
    pub leftovers: Vec<RelPath>,
}

impl Snapshot {
    pub fn file_count(&self) -> u64 {
        self.entries.iter().filter(|e| e.kind == Kind::File).count() as u64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum MoveKind {
    File {
        size: u64,
        mtime_ns: i64,
    },
    /// `files == 0` is a folder rename that only changes capital letters.
    Dir {
        files: u32,
        bytes: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Change {
    Create {
        path: RelPath,
        size: u64,
        mtime_ns: i64,
    },
    Update {
        path: RelPath,
        size: u64,
        mtime_ns: i64,
        replica_newer: bool,
    },
    /// Replica-only file: goes to the trash.
    Delete {
        path: RelPath,
        size: u64,
        mtime_ns: i64,
    },
    /// Renamed inside the replica instead of copied.
    Move {
        from: RelPath,
        to: RelPath,
        kind: MoveKind,
    },
    /// Empty source folder missing on the replica.
    MkDir { path: RelPath },
    /// Replica-only folder, removed once empty.
    RmDir { path: RelPath },
    /// Shown to the user, never applied.
    Skipped { path: RelPath, reason: String },
}

impl Change {
    /// The replica path this change writes or removes (the destination for a move).
    pub fn path(&self) -> &RelPath {
        match self {
            Change::Create { path, .. }
            | Change::Update { path, .. }
            | Change::Delete { path, .. }
            | Change::MkDir { path }
            | Change::RmDir { path }
            | Change::Skipped { path, .. } => path,
            Change::Move { to, .. } => to,
        }
    }

    pub fn bytes_to_copy(&self) -> u64 {
        match self {
            Change::Create { size, .. } | Change::Update { size, .. } => *size,
            _ => 0,
        }
    }
}

pub fn mtime_ns(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_nanos().min(i64::MAX as u128) as i64),
    }
}

pub fn system_time(ns: i64) -> SystemTime {
    if ns >= 0 {
        UNIX_EPOCH + Duration::from_nanos(ns as u64)
    } else {
        UNIX_EPOCH - Duration::from_nanos(ns.unsigned_abs())
    }
}

/// Modified time of `md` in nanoseconds; 0 when the filesystem has none.
pub fn modified_ns(md: &fs::Metadata) -> i64 {
    md.modified().map(mtime_ns).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_rejects_empty_and_dot_components() {
        for bad in ["", "/a", "a/", "a//b", ".", "..", "a/../b", "a/./b"] {
            assert!(RelPath::new(bad).is_err(), "{bad:?} should be rejected");
        }
        assert_eq!(RelPath::new("a/b c.txt").unwrap().as_str(), "a/b c.txt");
    }

    #[test]
    fn join_parent_name_depth() {
        let root = RelPath::root();
        let a = root.join("a");
        let ab = a.join("b.txt");
        assert_eq!(ab.as_str(), "a/b.txt");
        assert_eq!(ab.name(), "b.txt");
        assert_eq!(ab.parent(), Some(a.clone()));
        assert_eq!(a.parent(), Some(RelPath::root()));
        assert_eq!(root.parent(), None);
        assert_eq!((root.depth(), a.depth(), ab.depth()), (0, 1, 2));
        assert_eq!(a.join("x/y").as_str(), "a/x/y");
    }

    #[test]
    fn is_within_respects_component_boundaries() {
        let a = RelPath::new("a").unwrap();
        assert!(RelPath::new("a/b").unwrap().is_within(&a));
        assert!(a.is_within(&a));
        assert!(!RelPath::new("ab").unwrap().is_within(&a));
        assert!(a.is_within(&RelPath::root()));
        assert_eq!(RelPath::new("a/b/c").unwrap().strip_dir(&a), Some("b/c"));
        assert_eq!(RelPath::new("ab").unwrap().strip_dir(&a), None);
    }

    #[test]
    fn to_path_pushes_components() {
        let p = RelPath::new("a/b.txt").unwrap().to_path(Path::new("root"));
        assert_eq!(p, Path::new("root").join("a").join("b.txt"));
        assert_eq!(
            RelPath::root().to_path(Path::new("root")),
            Path::new("root")
        );
    }

    #[test]
    fn mtime_round_trips_before_and_after_epoch() {
        for ns in [0, 1_700_000_000_123_456_789, -86_400_000_000_000] {
            assert_eq!(mtime_ns(system_time(ns)), ns);
        }
    }

    #[test]
    fn relpath_serde_is_a_validated_string() {
        let p = RelPath::new("a/b").unwrap();
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"a/b\"");
        assert_eq!(serde_json::from_str::<RelPath>("\"a/b\"").unwrap(), p);
        assert!(serde_json::from_str::<RelPath>("\"../x\"").is_err());
    }

    #[test]
    fn change_path_and_bytes() {
        let p = RelPath::new("a").unwrap();
        let q = RelPath::new("b").unwrap();
        let mv = Change::Move {
            from: p.clone(),
            to: q.clone(),
            kind: MoveKind::File {
                size: 9,
                mtime_ns: 0,
            },
        };
        assert_eq!(mv.path(), &q);
        assert_eq!(mv.bytes_to_copy(), 0);
        let up = Change::Update {
            path: p,
            size: 7,
            mtime_ns: 0,
            replica_newer: false,
        };
        assert_eq!(up.bytes_to_copy(), 7);
    }
}
