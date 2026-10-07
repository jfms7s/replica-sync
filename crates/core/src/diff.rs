//! Compare two snapshots into the raw list of changes that make the replica match.

use crate::model::{Change, Entry, Kind, MTIME_TOLERANCE_NS, MoveKind, RelPath, Snapshot};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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

/// Keys at and under which nothing may change, with the reason shown to the user.
#[derive(Default)]
struct Blocks(BTreeMap<String, (RelPath, String)>);

impl Blocks {
    fn add(&mut self, k: String, rel: RelPath, reason: impl Into<String>) {
        self.0.entry(k).or_insert((rel, reason.into()));
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

pub fn diff(source: &Snapshot, replica: &Snapshot, case: CaseMode) -> Vec<Change> {
    let mut collisions = BTreeMap::new();
    let src = index(source, case, &mut collisions);
    let rep = index(replica, case, &mut BTreeMap::new());

    let mut blocks = Blocks::default();
    for (k, rel) in collisions {
        blocks.add(
            k,
            rel,
            "two names differ only by capital letters; the backup drive can't hold both",
        );
    }
    for p in &source.problems {
        blocks.add(
            key(&p.rel, case),
            p.rel.clone(),
            format!("could not read on the source: {}", p.reason),
        );
    }
    for p in &replica.problems {
        blocks.add(
            key(&p.rel, case),
            p.rel.clone(),
            format!("could not read on the replica: {}", p.reason),
        );
    }
    for (k, s) in &src {
        if s.kind == Kind::Link {
            blocks.add(k.clone(), s.rel.clone(), "link (not followed)");
        }
    }
    for (k, r) in &rep {
        if r.kind == Kind::Link {
            blocks.add(k.clone(), r.rel.clone(), "link (not followed)");
        } else if let Some(s) = src.get(k)
            && s.kind != r.kind
            && s.kind != Kind::Link
        {
            blocks.add(k.clone(), s.rel.clone(), "a file on one side and a folder on the other");
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
                Kind::Dir => out.push(Change::RmDir {
                    path: r.rel.clone(),
                }),
                Kind::Link => {}
            },
            (Some(s), Some(r)) => {
                if s.rel.name() != r.rel.name() {
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
    use crate::model::{Change, MoveKind, Problem};
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
            matches!(&out[0], Change::Skipped { reason, .. } if reason.contains("capital letters"))
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
                reason: "link (not followed)".into()
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
                reason: "a file on one side and a folder on the other".into(),
            }]
        );
    }

    #[test]
    fn unreadable_source_folder_blocks_deletes() {
        let mut src = snap(vec![dir("Docs")]);
        src.problems.push(Problem {
            rel: rel("Docs"),
            reason: "Permission denied".into(),
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
            reason: "boom".into(),
        });
        let out = diff(&src, &snap(vec![file("a", 1, T0)]), CaseMode::Sensitive);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Change::Skipped { .. }));
    }
}
