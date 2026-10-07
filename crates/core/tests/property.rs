mod common;

use common::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;

const NAMES: &[&str] = &["a", "b", "c"];

fn path() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(NAMES), 1..=3).prop_map(|p| p.join("/"))
}

/// rel → version. Content, size and mtime all derive from the version, so
/// "same size and mtime" really does mean "same content" (the spec's premise).
fn tree() -> impl Strategy<Value = BTreeMap<String, u8>> {
    prop::collection::btree_map(path(), 0u8..20, 0..12)
}

fn conflicts(k: &str, other: &str) -> bool {
    k.starts_with(&format!("{other}/")) || other.starts_with(&format!("{k}/"))
}

/// Source and replica trees with no file/folder clash on a shared name
/// (that case is Skipped by design and covered by unit tests), plus replica
/// folders that also hold a `Thumbs.db` (skipped by the built-in rules).
type Trees = (BTreeMap<String, u8>, BTreeMap<String, u8>, BTreeSet<String>);

fn trees() -> impl Strategy<Value = Trees> {
    let folder =
        prop::collection::vec(prop::sample::select(NAMES), 1..=2).prop_map(|p| p.join("/"));
    (tree(), tree(), prop::collection::vec(folder, 0..3)).prop_map(|(s, r, thumbs)| {
        let clean = |t: BTreeMap<String, u8>| -> BTreeMap<String, u8> {
            let keys: Vec<String> = t.keys().cloned().collect();
            t.into_iter()
                .filter(|(k, _)| !keys.iter().any(|o| conflicts(k, o)))
                .collect()
        };
        let s = clean(s);
        let r: BTreeMap<String, u8> = clean(r)
            .into_iter()
            .filter(|(k, _)| !s.keys().any(|o| conflicts(k, o)))
            .collect();
        // A folder can hold a Thumbs.db only where no file sits at or above it.
        let thumbs = thumbs
            .into_iter()
            .filter(|f| {
                !s.keys()
                    .chain(r.keys())
                    .any(|k| k == f || f.starts_with(&format!("{k}/")))
            })
            .collect();
        (s, r, thumbs)
    })
}

fn without_thumbs(t: BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    t.into_iter().filter(|(k, _)| !is_thumbs(k)).collect()
}

fn is_thumbs(k: &str) -> bool {
    k == THUMBS || k.ends_with(&format!("/{THUMBS}"))
}

const THUMBS: &str = "Thumbs.db";

fn content(v: u8) -> Vec<u8> {
    vec![b'a' + v; v as usize % 7 + 1]
}

fn materialise(root: &std::path::Path, t: &BTreeMap<String, u8>) {
    for (p, v) in t {
        write_file(root, p, &content(*v), T0 + i64::from(*v) * 10);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn full_sync_converges_and_never_loses_a_replica_byte((src, rep, thumbs) in trees()) {
        let (_d, s, r) = dirs();
        materialise(&s, &src);
        materialise(&r, &rep);
        for f in &thumbs {
            write_file(&r, &format!("{f}/{THUMBS}"), b"thumbnail cache", T0);
        }
        let before: Vec<Vec<u8>> = files(&r, true).into_values().collect();

        let (_, report) = sync_all(&s, &r);
        prop_assert!(report.stopped.is_none(), "{:?}", report.stopped);
        prop_assert_eq!(report.failed(), 0, "{:?}", report.results);
        prop_assert_eq!(without_thumbs(files(&r, true)), files(&s, true));
        prop_assert_eq!(
            files(&r, true).keys().filter(|k| is_thumbs(k)).count(),
            thumbs.len(),
            "every Thumbs.db stays in the replica"
        );
        let again = prepare_with(&s, &r, &[]);
        prop_assert!(again.plan.changes.is_empty(), "{:?}", again.plan.changes);

        let trash_root = r.join(replica_sync_core::trash::TRASH_DIR);
        let mut after: Vec<Vec<u8>> = files(&r, true).into_values()
            .chain(files(&trash_root, false).into_iter().filter(|(k, _)| !k.ends_with("manifest.jsonl")).map(|(_, v)| v))
            .collect();
        for b in before {
            let i = after.iter().position(|a| *a == b);
            prop_assert!(i.is_some(), "lost replica content {:?}", b);
            after.swap_remove(i.unwrap());
        }
        let _ = fs::remove_dir_all(&s);
    }
}
