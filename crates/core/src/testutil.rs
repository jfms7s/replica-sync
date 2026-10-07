#![allow(dead_code)]

//! Builders shared by the unit tests.

use crate::model::{Entry, Kind, RelPath, Snapshot};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// A fixed base time (seconds since the epoch) for test files.
pub const T0: i64 = 1_700_000_000;

pub fn ns(secs: i64) -> i64 {
    secs * 1_000_000_000
}

pub fn rel(s: &str) -> RelPath {
    RelPath::new(s).unwrap()
}

pub fn file(path: &str, size: u64, secs: i64) -> Entry {
    Entry {
        rel: rel(path),
        kind: Kind::File,
        size,
        mtime_ns: ns(secs),
    }
}

pub fn dir(path: &str) -> Entry {
    Entry {
        rel: rel(path),
        kind: Kind::Dir,
        size: 0,
        mtime_ns: 0,
    }
}

pub fn link(path: &str) -> Entry {
    Entry {
        rel: rel(path),
        kind: Kind::Link,
        size: 0,
        mtime_ns: 0,
    }
}

pub fn snap(mut entries: Vec<Entry>) -> Snapshot {
    entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    Snapshot {
        entries,
        ..Snapshot::default()
    }
}

/// Writes `content` at `root/rel` (creating parents) with mtime `secs`.
pub fn write_file(root: &Path, rel: &str, content: &[u8], secs: i64) {
    let p = RelPath::new(rel).unwrap().to_path(root);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(secs, 0)).unwrap();
}

/// Every file under `root` (excluding `.sync-trash`) as `rel → bytes`.
pub fn read_tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in fs::read_dir(dir).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_name() == ".sync-trash" {
                continue;
            }
            if e.file_type().unwrap().is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}
