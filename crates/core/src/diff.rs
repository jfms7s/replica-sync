//! Compare two snapshots into the raw list of changes that make the replica match.

use crate::model::{
    Change, Entry, Kind, MTIME_TOLERANCE_NS, MoveKind, Problem, ProblemKind, RelPath, SideKind,
    Snapshot,
};
use crate::reason::SkipReason;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ops::Bound;

/// Whether the replica's filesystem treats `A.jpg` and `a.jpg` as one name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum CaseMode {
    Sensitive,
    Insensitive,
}

/// The equality rule: same size and modified times within 2 seconds.
pub fn unchanged(src: &Entry, rep: &Entry) -> bool {
    src.size == rep.size && src.mtime_ns.abs_diff(rep.mtime_ns) <= MTIME_TOLERANCE_NS as u64
}

fn key(rel: &RelPath, case: CaseMode) -> String {
    match case {
        CaseMode::Sensitive => rel.as_str().to_owned(),
        CaseMode::Insensitive => rel.fold(),
    }
}

fn index<'a>(
    snap: &'a Snapshot,
    case: CaseMode,
    collisions: &mut BTreeMap<String, RelPath>,
) -> BTreeMap<String, &'a Entry> {
    let mut map = BTreeMap::new();
    for e in &snap.entries {
        let k = key(&e.rel, case);
        if map.insert(k.clone(), e).is_some() {
            collisions.entry(k).or_insert_with(|| e.rel.clone());
        }
    }
    map
}

fn problem_reason(p: &Problem, side: SideKind) -> SkipReason {
    match p.kind {
        ProblemKind::NotRegularFile => SkipReason::NotRegularFile,
        ProblemKind::ReservedName => SkipReason::ReservedName,
        ProblemKind::Unreadable => SkipReason::Unreadable {
            side,
            detail: p.detail.clone(),
        },
    }
}

/// Keys at and under which nothing may change, with the reason shown to the user.
#[derive(Default)]
struct Blocks(BTreeMap<String, (RelPath, SkipReason)>);

impl Blocks {
    fn add(&mut self, k: String, rel: RelPath, reason: SkipReason) {
        self.0.entry(k).or_insert((rel, reason));
    }

    fn has_blocked_ancestor(&self, k: &str) -> bool {
        self.0.contains_key("")
            || k.match_indices('/')
                .any(|(i, _)| self.0.contains_key(&k[..i]))
    }

    fn covers(&self, k: &str) -> bool {
        self.0.contains_key(k) || self.has_blocked_ancestor(k)
    }
}

fn has_children(map: &BTreeMap<String, &Entry>, k: &str) -> bool {
    let prefix = format!("{k}/");
    map.range::<str, _>((Bound::Included(prefix.as_str()), Bound::Unbounded))
        .next()
        .is_some_and(|(c, _)| c.starts_with(&prefix))
}

/// Replica folders an RmDir could never empty, because something the plan leaves
/// alone sits somewhere under them: a path the scan did not list (skip rule or
/// temp suffix), a leftover temp file the plan keeps, or a blocked key (which
/// covers replica links and replica problems too). Removing them would fail on
/// every sync.
fn pinned_folders(
    replica: &Snapshot,
    kept_leftovers: &[&Entry],
    blocks: &Blocks,
    case: CaseMode,
) -> HashSet<String> {
    let mut pinned = HashSet::new();
    let mut pin_ancestors = |k: &str| {
        for (i, _) in k.match_indices('/') {
            pinned.insert(k[..i].to_owned());
        }
    };
    for r in replica
        .unlisted
        .iter()
        .chain(kept_leftovers.iter().map(|e| &e.rel))
    {
        pin_ancestors(&key(r, case));
    }
    for k in blocks.0.keys() {
        pin_ancestors(k);
    }
    pinned
}

pub fn diff(source: &Snapshot, replica: &Snapshot, case: CaseMode) -> Vec<Change> {
    let mut collisions = BTreeMap::new();
    let src = index(source, case, &mut collisions);
    // A replica can hold two names one case-insensitive key can't tell apart
    // (a per-folder case-sensitive NTFS folder, a non-casefold ext4 subfolder).
    // `index` keeps only one of them, so the key is blocked like a source collision.
    let mut rep_collisions = BTreeMap::new();
    let rep = index(replica, case, &mut rep_collisions);

    let mut blocks = Blocks::default();
    for (k, rel) in collisions.into_iter().chain(rep_collisions) {
        blocks.add(k, rel, SkipReason::CaseCollision);
    }
    for p in &source.problems {
        blocks.add(
            key(&p.rel, case),
            p.rel.clone(),
            problem_reason(p, SideKind::Source),
        );
    }
    for p in &replica.problems {
        blocks.add(
            key(&p.rel, case),
            p.rel.clone(),
            problem_reason(p, SideKind::Replica),
        );
    }
    for (k, s) in &src {
        if s.kind == Kind::Link {
            blocks.add(k.clone(), s.rel.clone(), SkipReason::Link);
        }
    }
    for (k, r) in &rep {
        if r.kind == Kind::Link {
            blocks.add(k.clone(), r.rel.clone(), SkipReason::Link);
        } else if let Some(s) = src.get(k)
            && s.kind != r.kind
            && s.kind != Kind::Link
        {
            blocks.add(k.clone(), s.rel.clone(), SkipReason::FileVsFolder);
        }
    }

    let mut out = Vec::new();
    for (k, (rel, reason)) in &blocks.0 {
        if k.is_empty() || !blocks.has_blocked_ancestor(k) {
            out.push(Change::Skipped {
                path: rel.clone(),
                reason: reason.clone(),
            });
        }
    }

    // A replica leftover temp file is planned as an ordinary Delete (so it is
    // shown and goes to the trash), unless a block covers it or the same name is
    // on the source; then it is left alone and pins its folder.
    let src_leftovers: HashSet<String> =
        source.leftovers.iter().map(|e| key(&e.rel, case)).collect();
    // Two leftovers one key can't tell apart would collide in the trash too.
    let mut leftover_keys: BTreeMap<String, usize> = BTreeMap::new();
    for e in &replica.leftovers {
        *leftover_keys.entry(key(&e.rel, case)).or_default() += 1;
    }
    let (doomed, kept): (Vec<&Entry>, Vec<&Entry>) = replica.leftovers.iter().partition(|e| {
        let k = key(&e.rel, case);
        !blocks.covers(&k)
            && !src.contains_key(&k)
            && !rep.contains_key(&k)
            && !src_leftovers.contains(&k)
            && leftover_keys[&k] == 1
    });
    for e in doomed {
        out.push(Change::Delete {
            path: e.rel.clone(),
            size: e.size,
            mtime_ns: e.mtime_ns,
        });
    }

    let pinned = pinned_folders(replica, &kept, &blocks, case);
    let keys: BTreeSet<&String> = src.keys().chain(rep.keys()).collect();
    for k in keys {
        if blocks.covers(k) {
            continue;
        }
        match (src.get(k), rep.get(k)) {
            (Some(s), None) => match s.kind {
                Kind::File => out.push(Change::Create {
                    path: s.rel.clone(),
                    size: s.size,
                    mtime_ns: s.mtime_ns,
                }),
                Kind::Dir if !has_children(&src, k) => out.push(Change::MkDir {
                    path: s.rel.clone(),
                }),
                _ => {}
            },
            (None, Some(r)) => match r.kind {
                Kind::File => out.push(Change::Delete {
                    path: r.rel.clone(),
                    size: r.size,
                    mtime_ns: r.mtime_ns,
                }),
                Kind::Dir if !pinned.contains(k) => out.push(Change::RmDir {
                    path: r.rel.clone(),
                }),
                Kind::Dir | Kind::Link => {}
            },
            (Some(s), Some(r)) => {
                // A capital-letters rename of a folder would carry along whatever
                // the plan leaves alone under it (a blocked key, a hidden path).
                let carries_pinned = s.kind == Kind::Dir && pinned.contains(k);
                if s.rel.name() != r.rel.name() && !carries_pinned {
                    let kind = match s.kind {
                        Kind::Dir => MoveKind::Dir { files: 0, bytes: 0 },
                        _ => MoveKind::File {
                            size: s.size,
                            mtime_ns: s.mtime_ns,
                        },
                    };
                    out.push(Change::Move {
                        from: r.rel.clone(),
                        to: s.rel.clone(),
                        kind,
                    });
                }
                if s.kind == Kind::File && !unchanged(s, r) {
                    out.push(Change::Update {
                        path: s.rel.clone(),
                        size: s.size,
                        mtime_ns: s.mtime_ns,
                        replica_newer: r.mtime_ns > s.mtime_ns.saturating_add(MTIME_TOLERANCE_NS),
                    });
                }
            }
            (None, None) => unreachable!("key came from one of the maps"),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Change, MoveKind};
    use crate::testutil::{T0, dir, file, link, ns, rel, snap};

    fn d(src: Vec<Entry>, rep: Vec<Entry>) -> Vec<Change> {
        let mut out = diff(&snap(src), &snap(rep), CaseMode::Sensitive);
        out.sort_by(|a, b| a.path().cmp(b.path()));
        out
    }

    #[test]
    fn create_update_delete_and_unchanged() {
        let out = d(
            vec![
                file("new.txt", 1, T0),
                file("same.txt", 2, T0),
                file("changed.txt", 3, T0 + 60),
            ],
            vec![
                file("same.txt", 2, T0),
                file("changed.txt", 3, T0),
                file("gone.txt", 4, T0),
            ],
        );
        assert_eq!(
            out,
            vec![
                Change::Update {
                    path: rel("changed.txt"),
                    size: 3,
                    mtime_ns: ns(T0 + 60),
                    replica_newer: false
                },
                Change::Delete {
                    path: rel("gone.txt"),
                    size: 4,
                    mtime_ns: ns(T0)
                },
                Change::Create {
                    path: rel("new.txt"),
                    size: 1,
                    mtime_ns: ns(T0)
                },
            ]
        );
    }

    #[test]
    fn two_second_tolerance_boundary() {
        let src = Entry {
            mtime_ns: ns(T0) + MTIME_TOLERANCE_NS,
            ..file("a", 1, 0)
        };
        assert!(unchanged(&src, &file("a", 1, T0)));
        let src = Entry {
            mtime_ns: ns(T0) + MTIME_TOLERANCE_NS + 1,
            ..file("a", 1, 0)
        };
        assert!(!unchanged(&src, &file("a", 1, T0)));
        assert!(!unchanged(&file("a", 2, T0), &file("a", 1, T0)));
    }

    #[test]
    fn replica_newer_is_flagged_but_still_an_update() {
        let out = d(vec![file("a", 1, T0)], vec![file("a", 1, T0 + 100)]);
        assert!(matches!(
            out[0],
            Change::Update {
                replica_newer: true,
                ..
            }
        ));
    }

    #[test]
    fn mkdir_only_for_empty_source_folders() {
        let out = d(
            vec![dir("empty"), dir("full"), file("full/a", 1, T0)],
            vec![],
        );
        assert_eq!(
            out,
            vec![
                Change::MkDir { path: rel("empty") },
                Change::Create {
                    path: rel("full/a"),
                    size: 1,
                    mtime_ns: ns(T0)
                },
            ]
        );
    }

    #[test]
    fn replica_only_folder_is_rmdir_plus_deletes() {
        let out = d(vec![], vec![dir("old"), file("old/a", 1, T0)]);
        assert_eq!(
            out,
            vec![
                Change::RmDir { path: rel("old") },
                Change::Delete {
                    path: rel("old/a"),
                    size: 1,
                    mtime_ns: ns(T0)
                },
            ]
        );
    }

    #[test]
    fn case_only_rename_on_insensitive_drive() {
        let src = snap(vec![file("photo.jpg", 5, T0)]);
        let same = diff(
            &src,
            &snap(vec![file("Photo.JPG", 5, T0)]),
            CaseMode::Insensitive,
        );
        assert_eq!(
            same,
            vec![Change::Move {
                from: rel("Photo.JPG"),
                to: rel("photo.jpg"),
                kind: MoveKind::File {
                    size: 5,
                    mtime_ns: ns(T0)
                },
            }]
        );
        let changed = diff(
            &src,
            &snap(vec![file("Photo.JPG", 4, T0)]),
            CaseMode::Insensitive,
        );
        assert!(matches!(changed[0], Change::Move { .. }));
        assert!(matches!(changed[1], Change::Update { .. }));
        let sensitive = diff(
            &src,
            &snap(vec![file("Photo.JPG", 5, T0)]),
            CaseMode::Sensitive,
        );
        assert_eq!(sensitive.len(), 2); // Create + Delete
    }

    #[test]
    fn folder_case_rename_does_not_rename_children() {
        let out = diff(
            &snap(vec![dir("photos"), file("photos/a.jpg", 1, T0)]),
            &snap(vec![dir("Photos"), file("Photos/a.jpg", 1, T0)]),
            CaseMode::Insensitive,
        );
        assert_eq!(
            out,
            vec![Change::Move {
                from: rel("Photos"),
                to: rel("photos"),
                kind: MoveKind::Dir { files: 0, bytes: 0 },
            }]
        );
    }

    #[test]
    fn source_names_colliding_by_case_are_skipped_and_replica_kept() {
        let out = diff(
            &snap(vec![file("A.jpg", 1, T0), file("a.jpg", 2, T0)]),
            &snap(vec![file("a.jpg", 9, T0)]),
            CaseMode::Insensitive,
        );
        assert_eq!(out.len(), 1);
        assert!(
            matches!(&out[0], Change::Skipped { reason, .. } if *reason == SkipReason::CaseCollision)
        );
    }

    #[test]
    fn links_block_their_path_on_both_sides() {
        let out = d(
            vec![link("alias")],
            vec![dir("alias"), file("alias/x", 1, T0)],
        );
        assert_eq!(
            out,
            vec![Change::Skipped {
                path: rel("alias"),
                reason: SkipReason::Link
            }]
        );
    }

    #[test]
    fn file_vs_folder_is_skipped_and_nothing_under_it_changes() {
        let out = d(vec![file("x", 1, T0)], vec![dir("x"), file("x/y", 1, T0)]);
        assert_eq!(
            out,
            vec![Change::Skipped {
                path: rel("x"),
                reason: SkipReason::FileVsFolder,
            }]
        );
    }

    #[test]
    fn unreadable_source_folder_blocks_deletes() {
        let mut src = snap(vec![dir("Docs")]);
        src.problems.push(Problem {
            rel: rel("Docs"),
            kind: ProblemKind::Unreadable,
            detail: "Permission denied".into(),
        });
        let rep = snap(vec![
            dir("Docs"),
            file("Docs/a.txt", 1, T0),
            file("Docs/sub/b.txt", 1, T0),
        ]);
        let out = diff(&src, &rep, CaseMode::Sensitive);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Change::Skipped { path, .. } if path.as_str() == "Docs"));
    }

    #[test]
    fn problem_at_root_blocks_everything() {
        let mut src = snap(vec![]);
        src.problems.push(Problem {
            rel: RelPath::root(),
            kind: ProblemKind::Unreadable,
            detail: "boom".into(),
        });
        let out = diff(&src, &snap(vec![file("a", 1, T0)]), CaseMode::Sensitive);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Change::Skipped { .. }));
    }
    #[test]
    fn replica_only_folder_holding_an_unlisted_entry_is_not_removed() {
        let mut rep = snap(vec![
            dir("OldTrip"),
            file("OldTrip/p.jpg", 1, T0),
            dir("Deep"),
            dir("Deep/sub"),
            dir("Gone"),
            file("Gone/q.jpg", 1, T0),
        ]);
        rep.unlisted.push(rel("OldTrip/Thumbs.db"));
        rep.unlisted.push(rel("Deep/sub/Temp"));
        let out = diff(&snap(vec![]), &rep, CaseMode::Sensitive);
        let rmdirs: Vec<&str> = out
            .iter()
            .filter_map(|c| match c {
                Change::RmDir { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rmdirs, vec!["Gone"]);
        assert!(
            out.iter().any(
                |c| matches!(c, Change::Delete { path, .. } if path.as_str() == "OldTrip/p.jpg")
            )
        );
    }

    #[test]
    fn leftovers_links_problems_and_blocks_keep_their_replica_folders() {
        let mut rep = snap(vec![
            dir("L"),
            dir("K"),
            link("K/alias"),
            dir("P"),
            dir("B"),
            dir("B/x"),
            dir("Free"),
        ]);
        rep.unlisted.push(rel("L/a.jpg.replica-sync.tmp")); // e.g. a folder with the suffix
        rep.problems.push(Problem {
            rel: rel("P/locked.bin"),
            kind: ProblemKind::Unreadable,
            detail: "Permission denied".into(),
        });
        let src = snap(vec![file("B/x", 1, T0)]); // file vs folder: blocked key B/x
        let out = diff(&src, &rep, CaseMode::Sensitive);
        let rmdirs: Vec<&str> = out
            .iter()
            .filter_map(|c| match c {
                Change::RmDir { path } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(rmdirs, vec!["Free"]);
    }

    fn deletes(out: &[Change]) -> Vec<&str> {
        out.iter()
            .filter_map(|c| match c {
                Change::Delete { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_replica_case_collision_is_skipped_and_nothing_changes_there() {
        let rep = snap(vec![
            dir("Photos"),
            file("Photos/A.jpg", 1, T0),
            file("Photos/a.jpg", 2, T0),
        ]);
        let out = diff(&snap(vec![]), &rep, CaseMode::Insensitive);
        let skipped: Vec<_> = out
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    Change::Skipped {
                        reason: SkipReason::CaseCollision,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(skipped.len(), 1, "{out:?}");
        assert!(deletes(&out).is_empty(), "{out:?}");
        assert!(
            !out.iter().any(|c| matches!(c, Change::RmDir { .. })),
            "{out:?}"
        );
    }

    #[test]
    fn a_replica_case_collision_blocks_updates_at_that_key() {
        let src = snap(vec![file("a.jpg", 9, T0 + 100)]);
        let rep = snap(vec![file("A.jpg", 1, T0), file("a.jpg", 2, T0)]);
        let out = diff(&src, &rep, CaseMode::Insensitive);
        assert!(
            out.iter().all(|c| matches!(
                c,
                Change::Skipped {
                    reason: SkipReason::CaseCollision,
                    ..
                }
            )),
            "{out:?}"
        );
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn a_folder_holding_a_replica_case_collision_is_not_renamed() {
        let src = snap(vec![dir("photos")]);
        let rep = snap(vec![
            dir("Photos"),
            file("Photos/A.jpg", 1, T0),
            file("Photos/a.jpg", 2, T0),
        ]);
        let out = diff(&src, &rep, CaseMode::Insensitive);
        assert!(
            !out.iter().any(|c| matches!(c, Change::Move { .. })),
            "{out:?}"
        );
    }

    #[test]
    fn case_twin_folders_on_the_replica_block_their_subtrees() {
        let src = snap(vec![dir("x"), file("x/f", 1, T0)]);
        let rep = snap(vec![
            dir("X"),
            dir("x"),
            file("X/g", 1, T0),
            file("x/f", 1, T0),
        ]);
        let out = diff(&src, &rep, CaseMode::Insensitive);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(matches!(
            &out[0],
            Change::Skipped {
                reason: SkipReason::CaseCollision,
                ..
            }
        ));
    }

    #[test]
    fn replica_case_collisions_change_nothing_in_sensitive_mode() {
        let rep = snap(vec![file("A.jpg", 1, T0), file("a.jpg", 2, T0)]);
        let out = diff(&snap(vec![]), &rep, CaseMode::Sensitive);
        assert_eq!(deletes(&out), vec!["A.jpg", "a.jpg"]);
    }

    #[test]
    fn case_twin_leftovers_are_both_kept() {
        let mut rep = snap(vec![dir("L")]);
        rep.leftovers.push(file("L/A.replica-sync.tmp", 1, T0));
        rep.leftovers.push(file("L/a.replica-sync.tmp", 1, T0));
        let out = diff(&snap(vec![]), &rep, CaseMode::Insensitive);
        assert!(deletes(&out).is_empty(), "{out:?}");
        assert!(
            !out.iter().any(|c| matches!(c, Change::RmDir { .. })),
            "{out:?}"
        );
    }

    #[test]
    fn a_reserved_name_folder_is_skipped_and_nothing_under_it_changes() {
        let reserved = |p: &str| Problem {
            rel: rel(p),
            kind: ProblemKind::ReservedName,
            detail: String::new(),
        };
        let mut src = snap(vec![dir("Docs")]);
        src.problems.push(reserved("Docs/old.replica-sync.tmp"));
        let mut rep = snap(vec![dir("Gone")]);
        rep.problems.push(reserved("Gone/d.replica-sync.tmp"));
        let out = diff(&src, &rep, CaseMode::Sensitive);
        assert!(out.contains(&Change::Skipped {
            path: rel("Docs/old.replica-sync.tmp"),
            reason: SkipReason::ReservedName,
        }));
        assert!(out.contains(&Change::Skipped {
            path: rel("Gone/d.replica-sync.tmp"),
            reason: SkipReason::ReservedName,
        }));
        assert!(
            !out.iter()
                .any(|c| matches!(c, Change::RmDir { .. } | Change::Delete { .. })),
            "{out:?}"
        );
    }

    #[test]
    fn a_leftover_is_planned_as_a_delete_and_frees_its_folder() {
        let mut rep = snap(vec![dir("Trip")]);
        rep.leftovers
            .push(file("Trip/b.jpg.replica-sync.tmp", 7, T0));
        let out = diff(&snap(vec![]), &rep, CaseMode::Sensitive);
        assert!(out.contains(&Change::Delete {
            path: rel("Trip/b.jpg.replica-sync.tmp"),
            size: 7,
            mtime_ns: ns(T0),
        }));
        assert!(out.contains(&Change::RmDir { path: rel("Trip") }));
    }

    #[test]
    fn a_blocked_or_source_side_leftover_is_left_alone_and_pins_its_folder() {
        // S is a link on the source (blocked); O holds a temp-named file on both sides.
        let mut rep = snap(vec![dir("S"), dir("O")]);
        rep.leftovers.push(file("S/t.replica-sync.tmp", 1, T0));
        rep.leftovers.push(file("O/t.replica-sync.tmp", 1, T0));
        let mut src = snap(vec![link("S")]);
        src.leftovers.push(file("O/t.replica-sync.tmp", 1, T0));
        let out = diff(&src, &rep, CaseMode::Sensitive);
        assert!(deletes(&out).is_empty(), "{out:?}");
        assert!(
            !out.iter()
                .any(|c| matches!(c, Change::RmDir { path } if path.as_str() == "O")),
            "{out:?}"
        );
    }
}
