//! `pairs.json` is a format on the user's machine, written by every release.
//! These tests pin what an upgrade does to it, with a store a real v0.2.0
//! wrote (`fixtures/pairs-v0.2.0.json`, produced by v0.2.0's own `PairStore`).

use replica_sync_core::pairs::{Pair, PairStore};
use std::fs;
use std::path::PathBuf;

const V0_2_0: &str = include_str!("fixtures/pairs-v0.2.0.json");

fn store_with(text: &str) -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("pairs.json");
    fs::write(&path, text).unwrap();
    (d, path)
}

fn pair<'a>(store: &'a PairStore, name: &str) -> &'a Pair {
    store.pairs.iter().find(|p| p.name == name).unwrap()
}

#[test]
fn a_v0_2_0_store_loads_and_loading_does_not_write() {
    let (d, path) = store_with(V0_2_0);
    let store = PairStore::load(&path).unwrap();
    assert_eq!(store.pairs.len(), 2);
    assert_eq!(fs::read_to_string(&path).unwrap(), V0_2_0);
    let entries: Vec<_> = fs::read_dir(d.path()).unwrap().collect();
    assert_eq!(entries.len(), 1, "no temp file or backup next to the store");
}

#[test]
fn an_old_stores_history_lands_on_its_single_replica() {
    let (_d, path) = store_with(V0_2_0);
    let store = PairStore::load(&path).unwrap();

    let photos = pair(&store, "Photos");
    assert_eq!(photos.replicas().len(), 1);
    let r = &photos.replicas()[0];
    assert_eq!(r.side.volume_id, "win:00000000a1b2c3d4");
    assert_eq!(r.side.rel_path.as_str(), "Backup/Photos");
    assert_eq!(r.side.label, "BACKUP A");
    let last = r
        .last_sync
        .as_ref()
        .expect("last_sync moved to the replica");
    assert_eq!(
        (last.at.as_str(), last.applied, last.failed, last.stopped),
        ("2026-10-07T18:22:41Z", 412, 1, true)
    );
    assert_eq!(r.last_scan_files, Some(18230));
    assert!(!r.is_first_sync());

    // A pair that never synced stays a first sync.
    let docs = pair(&store, "Documents (never synced)");
    assert_eq!(docs.replicas().len(), 1);
    assert!(docs.replicas()[0].is_first_sync());
    assert_eq!(docs.replicas()[0].last_scan_files, None);
    assert!(docs.replicas()[0].side.rel_path.is_root());
    assert_eq!(docs.trash_days, 7);
    assert!(docs.user_rules.is_empty());
}

#[test]
fn an_old_store_saves_as_format_2_and_round_trips() {
    let (_d, path) = store_with(V0_2_0);
    let store = PairStore::load(&path).unwrap();
    store.save().unwrap();

    let saved = fs::read_to_string(&path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(json["version"], 2);
    for p in json["pairs"].as_array().unwrap() {
        assert!(p.get("replica").is_none(), "old shape written: {p}");
        assert!(
            p.get("last_sync").is_none(),
            "history left on the pair: {p}"
        );
        assert_eq!(p["replicas"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        json["pairs"][0]["replicas"][0]["last_sync"]["applied"], 412,
        "history survived the move"
    );

    let again = PairStore::load(&path).unwrap();
    assert_eq!(again.pairs, store.pairs);
    again.save().unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        saved,
        "stable once migrated"
    );
}

/// The reader main had before this change (`PairStore::load` and its types as
/// of v0.7.0), kept verbatim so the one-way store is tested against what a
/// user who downgrades would run.
mod v0_7_0_reader {
    use serde::Deserialize;
    use std::io;
    use std::path::Path;

    const STORE_VERSION: u32 = 1;

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Side {
        volume_id: String,
        rel_path: String,
        label: String,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct LastSync {
        at: String,
        applied: usize,
        failed: usize,
        #[serde(default)]
        stopped: bool,
    }

    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct Pair {
        id: String,
        name: String,
        source: Side,
        replica: Side,
        user_rules: Vec<String>,
        trash_days: u32,
        last_sync: Option<LastSync>,
        last_scan_files: Option<u64>,
    }

    #[derive(Deserialize)]
    struct StoreIn {
        #[serde(default = "first_store_version")]
        version: u32,
        pairs: Vec<Pair>,
    }

    fn first_store_version() -> u32 {
        1
    }

    pub fn load(path: &Path) -> io::Result<usize> {
        let text = std::fs::read_to_string(path)?;
        let store = serde_json::from_str::<StoreIn>(&text).map_err(io::Error::other)?;
        if store.version > STORE_VERSION {
            return Err(io::Error::other("written by a newer replica-sync"));
        }
        Ok(store.pairs.len())
    }
}

#[test]
fn the_reader_before_this_change_refuses_a_format_2_store() {
    let (_d, path) = store_with(V0_2_0);
    // The frozen reader does read what it always read ...
    assert_eq!(v0_7_0_reader::load(&path).unwrap(), 2);
    // ... but not what this build writes, so the app that has it shows the
    // Broken screen (`store.unreadable`, nothing is ever written then).
    PairStore::load(&path).unwrap().save().unwrap();
    let saved = fs::read(&path).unwrap();
    assert!(v0_7_0_reader::load(&path).is_err());
    assert_eq!(
        fs::read(&path).unwrap(),
        saved,
        "the failed read left the file as it was"
    );
}
