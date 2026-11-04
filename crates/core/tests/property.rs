mod common;

use common::*;
use proptest::prelude::*;
use replica_sync_core::execute::{Control, Progress};
use replica_sync_core::model::{CaseMode, Change};
use replica_sync_core::plan::ChangeId;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

const NAMES: &[&str] = &["a", "b", "c"];
/// Adds capitals, for the case-insensitive properties.
const MIXED: &[&str] = &["a", "b", "c", "A", "B"];

/// 64 cases by default, so CI time stays flat; `PROPTEST_CASES=512` raises it
/// (e.g. for a nightly run) without a code change.
fn config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(64);
    ProptestConfig::with_cases(cases)
}

fn path(names: &'static [&'static str]) -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(names), 1..=3).prop_map(|p| p.join("/"))
}

/// rel → version. Size and mtime derive from the version only, so "same size
/// and mtime" means "same version" (the spec's premise); the bytes also carry
/// the path, so every file in a tree is unique and the no-loss check can follow
/// each one (see [`content`]).
fn tree(names: &'static [&'static str]) -> impl Strategy<Value = BTreeMap<String, u8>> {
    prop::collection::btree_map(path(names), 0u8..20, 0..12)
}

/// `a`, `a/b`, `a/b/c` for `a/b/c`.
fn prefixes(p: &str) -> Vec<String> {
    let parts: Vec<&str> = p.split('/').collect();
    (1..=parts.len()).map(|n| parts[..n].join("/")).collect()
}

fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// A file/folder clash on a shared name (compared folded under `Insensitive`).
fn conflicts(k: &str, other: &str, case: CaseMode) -> bool {
    let (k, other) = match case {
        CaseMode::Sensitive => (k.to_owned(), other.to_owned()),
        CaseMode::Insensitive => (fold(k), fold(other)),
    };
    k.starts_with(&format!("{other}/")) || other.starts_with(&format!("{k}/"))
}

/// Drops entries that clash with another entry of the same tree: a file/folder
/// clash, and under `Insensitive` two names (or folder names on the way) that
/// differ only in capitals. On a real case-insensitive drive the second can't
/// exist on the replica, and on the source it is a `Skipped` collision; both
/// have unit tests. On a Linux tmpfs both names really exist, so filtering is
/// what keeps the property honest.
fn clean(t: BTreeMap<String, u8>, case: CaseMode) -> BTreeMap<String, u8> {
    let keys: Vec<String> = t.keys().cloned().collect();
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut out = BTreeMap::new();
    for (k, v) in t {
        if keys.iter().any(|o| conflicts(&k, o, case)) {
            continue;
        }
        if case == CaseMode::Insensitive {
            let prefixes = prefixes(&k);
            let consistent = prefixes
                .iter()
                .all(|p| seen.get(&fold(p)).is_none_or(|exact| exact == p));
            if !consistent {
                continue;
            }
            for p in prefixes {
                seen.insert(fold(&p), p);
            }
        }
        out.insert(k, v);
    }
    out
}

/// True when a folder on the way to `k` matches a source folder only when
/// capitals are ignored. Only on a case-sensitive disk run as `Insensitive`
/// (Linux CI): after the engine renames `B` to `b`, a later change still
/// spelled `B/x` finds the file on a real case-insensitive drive but not on
/// tmpfs, so the simulation would report a failure no real drive has. Windows
/// CI runs on NTFS and keeps these cases.
fn folder_case_differs(k: &str, source: &BTreeMap<String, u8>, case: CaseMode) -> bool {
    if cfg!(windows) || case == CaseMode::Sensitive {
        return false;
    }
    let folders = |p: &str| -> Vec<String> {
        let parts: Vec<&str> = p.split('/').collect();
        (1..parts.len()).map(|n| parts[..n].join("/")).collect()
    };
    let mine = folders(k);
    source
        .keys()
        .flat_map(|o| folders(o))
        .any(|f| mine.iter().any(|m| fold(m) == fold(&f) && *m != f))
}

/// Source and replica trees with no file/folder clash on a shared name
/// (that case is Skipped by design and covered by unit tests), plus replica
/// folders that also hold a `Thumbs.db` (skipped by the built-in rules).
type Trees = (BTreeMap<String, u8>, BTreeMap<String, u8>, BTreeSet<String>);

fn trees_in(names: &'static [&'static str], case: CaseMode) -> impl Strategy<Value = Trees> {
    let folder =
        prop::collection::vec(prop::sample::select(names), 1..=2).prop_map(|p| p.join("/"));
    (
        tree(names),
        tree(names),
        prop::collection::vec(folder, 0..3),
    )
        .prop_map(move |(s, r, thumbs)| {
            let s = clean(s, case);
            let r: BTreeMap<String, u8> = clean(r, case)
                .into_iter()
                .filter(|(k, _)| !s.keys().any(|o| conflicts(k, o, case)))
                .filter(|(k, _)| !folder_case_differs(k, &s, case))
                .collect();
            // A folder can hold a Thumbs.db only where no file sits at or above
            // it. Under `Insensitive` its folders must also be spelled like the
            // replica's (and, on Linux, the source's) and like each other.
            let mut known: HashMap<String, String> = HashMap::new();
            let sides: Vec<&String> = if case == CaseMode::Insensitive && !cfg!(windows) {
                r.keys().chain(s.keys()).collect()
            } else {
                r.keys().collect()
            };
            for k in sides {
                for p in prefixes(k) {
                    known.entry(fold(&p)).or_insert(p);
                }
            }
            let mut thumbs_out = BTreeSet::new();
            for f in thumbs {
                let clash = s.keys().chain(r.keys()).any(|k| {
                    let (k, f) = match case {
                        CaseMode::Sensitive => (k.clone(), f.clone()),
                        CaseMode::Insensitive => (fold(k), fold(&f)),
                    };
                    k == f || f.starts_with(&format!("{k}/"))
                });
                let spelled = case == CaseMode::Sensitive
                    || prefixes(&f)
                        .iter()
                        .all(|p| known.get(&fold(p)).is_none_or(|e| e == p));
                if !clash && spelled {
                    for p in prefixes(&f) {
                        known.entry(fold(&p)).or_insert(p);
                    }
                    thumbs_out.insert(f);
                }
            }
            let thumbs = thumbs_out;
            (s, r, thumbs)
        })
}

fn trees() -> impl Strategy<Value = Trees> {
    trees_in(NAMES, CaseMode::Sensitive)
}

fn without_thumbs<V>(t: BTreeMap<String, V>) -> BTreeMap<String, V> {
    t.into_iter().filter(|(k, _)| !is_thumbs(k)).collect()
}

fn is_thumbs(k: &str) -> bool {
    k == THUMBS || k.ends_with(&format!("/{THUMBS}"))
}

const THUMBS: &str = "Thumbs.db";

/// `"<path>:<version>"` padded to a length that depends on the version only:
/// unique per file, while size and mtime still say only "which version".
fn content(p: &str, v: u8) -> Vec<u8> {
    let mut b = format!("{p}:{v}").into_bytes();
    b.resize(12 + v as usize % 7, b'.');
    b
}

fn materialise(root: &Path, t: &BTreeMap<String, u8>) {
    for (p, v) in t {
        write_file(root, p, &content(p, *v), T0 + i64::from(*v) * 10);
    }
}

fn setup(
    src: &BTreeMap<String, u8>,
    rep: &BTreeMap<String, u8>,
    thumbs: &BTreeSet<String>,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let (d, s, r) = dirs();
    materialise(&s, src);
    materialise(&r, rep);
    for f in thumbs {
        write_file(&r, &format!("{f}/{THUMBS}"), b"thumbnail cache", T0);
    }
    (d, s, r)
}

/// rel → size: what "the replica matches the source" means to the engine
/// (mtime is covered by the empty re-plan). Bytes may differ where a move or
/// a capitals-only rename kept the replica's own copy of the same version.
fn sizes(t: BTreeMap<String, Vec<u8>>) -> BTreeMap<String, usize> {
    t.into_iter().map(|(k, v)| (k, v.len())).collect()
}

/// Every file the replica held before is still in the replica or in one of
/// its trash runs, byte for byte. Contents are unique per file (bar the
/// `Thumbs.db` copies), so this follows each file rather than a multiset.
fn assert_nothing_lost(r: &Path, before: Vec<Vec<u8>>) -> Result<(), TestCaseError> {
    let trash_root = r.join(replica_sync_core::trash::TRASH_DIR);
    let mut after: Vec<Vec<u8>> = files(r, true)
        .into_values()
        .chain(
            files(&trash_root, false)
                .into_iter()
                .filter(|(k, _)| !k.ends_with("manifest.jsonl"))
                .map(|(_, v)| v),
        )
        .collect();
    for b in before {
        let i = after.iter().position(|a| *a == b);
        prop_assert!(
            i.is_some(),
            "lost replica content {:?}",
            String::from_utf8_lossy(&b)
        );
        after.swap_remove(i.unwrap());
    }
    Ok(())
}

/// A full sync from here converges in one pass and the next plan is empty.
fn assert_converges(s: &Path, r: &Path, case: CaseMode) -> Result<(), TestCaseError> {
    let p = prepare_with_case(s, r, &[], case);
    let report = apply_all(s, r, &p);
    prop_assert!(report.stopped.is_none(), "{:?}", report.stopped);
    prop_assert_eq!(report.failed(), 0, "{:?}", report.results);
    let key = |t: BTreeMap<String, usize>| -> BTreeMap<String, usize> {
        match case {
            CaseMode::Sensitive => t,
            // A folder holding skipped files (a `Thumbs.db`) keeps its replica
            // spelling by design (`diff` won't rename it and carry them along),
            // so on an insensitive drive only the folded names must match.
            CaseMode::Insensitive => t.into_iter().map(|(k, v)| (fold(&k), v)).collect(),
        }
    };
    prop_assert_eq!(
        key(sizes(without_thumbs(files(r, true)))),
        key(sizes(files(s, true)))
    );
    let again = prepare_with_case(s, r, &[], case);
    prop_assert!(again.plan.is_empty(), "{:?}", again.plan.changes);
    Ok(())
}

/// A random subset of `ids`, chosen by `mask` (cycled).
fn pick(ids: &HashSet<ChangeId>, mask: &[bool]) -> HashSet<ChangeId> {
    let mut sorted: Vec<ChangeId> = ids.iter().copied().collect();
    sorted.sort_by_key(|id| id.0);
    sorted
        .into_iter()
        .enumerate()
        .filter(|(i, _)| mask[i % mask.len()])
        .map(|(_, id)| id)
        .collect()
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn full_sync_converges_and_never_loses_a_replica_byte((src, rep, thumbs) in trees()) {
        let (_d, s, r) = setup(&src, &rep, &thumbs);
        let before: Vec<Vec<u8>> = files(&r, true).into_values().collect();

        let (_, report) = sync_all(&s, &r);
        prop_assert!(report.stopped.is_none(), "{:?}", report.stopped);
        prop_assert_eq!(report.failed(), 0, "{:?}", report.results);
        prop_assert_eq!(sizes(without_thumbs(files(&r, true))), sizes(files(&s, true)));
        prop_assert_eq!(
            files(&r, true).keys().filter(|k| is_thumbs(k)).count(),
            thumbs.len(),
            "every Thumbs.db stays in the replica"
        );
        let again = prepare_with(&s, &r, &[]);
        prop_assert!(again.plan.changes.is_empty(), "{:?}", again.plan.changes);
        assert_nothing_lost(&r, before)?;
    }

    /// Mixed-case names on a replica treated as case-insensitive (as on
    /// Windows): capitals-only renames, folder renames and moves that differ in
    /// case, all forced on Linux by passing `CaseMode::Insensitive`.
    #[test]
    fn mixed_case_full_sync_converges_on_an_insensitive_replica(
        (src, rep, thumbs) in trees_in(MIXED, CaseMode::Insensitive)
    ) {
        let (_d, s, r) = setup(&src, &rep, &thumbs);
        let before: Vec<Vec<u8>> = files(&r, true).into_values().collect();
        assert_converges(&s, &r, CaseMode::Insensitive)?;
        prop_assert_eq!(
            files(&r, true).keys().filter(|k| is_thumbs(k)).count(),
            thumbs.len(),
            "every Thumbs.db stays in the replica"
        );
        assert_nothing_lost(&r, before)?;
    }

    /// Approving only some changes applies exactly those: an unapproved delete
    /// or update leaves the replica file as it was, an unapproved create leaves
    /// nothing, and a later full sync still converges without loss.
    #[test]
    fn partial_approval_applies_only_what_was_approved(
        (src, rep, thumbs) in trees(),
        mask in prop::collection::vec(any::<bool>(), 1..16),
    ) {
        let (_d, s, r) = setup(&src, &rep, &thumbs);
        let before_map = files(&r, true);
        let before: Vec<Vec<u8>> = before_map.values().cloned().collect();

        let p = prepare_with(&s, &r, &[]);
        let approved = pick(&p.plan.actionable_ids(), &mask);
        let report = apply_some(&s, &r, &p, &approved, &Control::default(), &mut |_| {});
        prop_assert!(report.stopped.is_none(), "{:?}", report.stopped);
        prop_assert_eq!(report.failed(), 0, "{:?}", report.results);
        let now = files(&r, true);
        let source = files(&s, true);
        for planned in &p.plan.changes {
            let approved = approved.contains(&planned.id);
            match &planned.change {
                Change::Delete { path, .. } | Change::Update { path, .. } if !approved => {
                    prop_assert_eq!(now.get(path.as_str()), before_map.get(path.as_str()), "unapproved {:?}", planned.change);
                }
                Change::Create { path, .. } if !approved => {
                    prop_assert!(!now.contains_key(path.as_str()), "unapproved {:?}", planned.change);
                }
                Change::Create { path, .. } | Change::Update { path, .. } if approved => {
                    prop_assert_eq!(now.get(path.as_str()), source.get(path.as_str()), "approved {:?}", planned.change);
                }
                Change::Delete { path, .. } if approved => {
                    prop_assert!(!now.contains_key(path.as_str()), "approved {:?}", planned.change);
                }
                _ => {}
            }
        }
        assert_nothing_lost(&r, before.clone())?;
        assert_converges(&s, &r, CaseMode::Sensitive)?;
        assert_nothing_lost(&r, before)?;
    }

    /// Cancelling after k changes, then syncing everything, converges and
    /// loses nothing (a second run in the same second gets its own trash folder).
    #[test]
    fn cancel_then_resume_converges_without_loss(
        (src, rep, thumbs) in trees(),
        k in 0usize..12,
    ) {
        let (_d, s, r) = setup(&src, &rep, &thumbs);
        let before: Vec<Vec<u8>> = files(&r, true).into_values().collect();

        let p = prepare_with(&s, &r, &[]);
        let control = Control::default();
        let report = apply_some(&s, &r, &p, &p.plan.actionable_ids(), &control, &mut |pr: &Progress| {
            if pr.changes_done == k {
                control.cancel();
            }
        });
        prop_assert_eq!(report.failed(), 0, "{:?}", report.results);
        assert_nothing_lost(&r, before.clone())?;
        assert_converges(&s, &r, CaseMode::Sensitive)?;
        assert_nothing_lost(&r, before)?;
    }
}
