//! Turn Delete+Create pairs into renames, and whole moved folders into one rename.

use crate::diff::CaseMode;
use crate::model::{Change, Kind, MTIME_TOLERANCE_NS, MoveKind, RelPath, Snapshot};
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn detect_moves(
    changes: Vec<Change>,
    source: &Snapshot,
    replica: &Snapshot,
    case: CaseMode,
) -> Vec<Change> {
    let changes = pair_file_moves(changes);
    collapse_folder_moves(changes, source, replica, case)
}

fn pair_file_moves(changes: Vec<Change>) -> Vec<Change> {
    let pairs: Vec<(usize, usize)> = {
        let mut creates: HashMap<(&str, u64), Vec<usize>> = HashMap::new();
        let mut deletes: HashMap<(&str, u64), Vec<usize>> = HashMap::new();
        for (i, c) in changes.iter().enumerate() {
            match c {
                Change::Create { path, size, .. } => {
                    creates.entry((path.name(), *size)).or_default().push(i)
                }
                Change::Delete { path, size, .. } => {
                    deletes.entry((path.name(), *size)).or_default().push(i)
                }
                _ => {}
            }
        }
        let mtime = |i: usize| match &changes[i] {
            Change::Create { mtime_ns, .. } | Change::Delete { mtime_ns, .. } => *mtime_ns,
            _ => unreachable!("only creates and deletes are indexed"),
        };
        let close = |a: usize, b: usize| mtime(a).abs_diff(mtime(b)) <= MTIME_TOLERANCE_NS as u64;
        let mut pairs = Vec::new();
        for (key, dels) in &deletes {
            let Some(crs) = creates.get(key) else {
                continue;
            };
            for &d in dels {
                let mut cands = crs.iter().copied().filter(|&c| close(d, c));
                let (Some(c), None) = (cands.next(), cands.next()) else {
                    continue;
                };
                if dels.iter().filter(|&&other| close(other, c)).count() == 1 {
                    pairs.push((d, c));
                }
            }
        }
        pairs.sort_unstable();
        pairs
    };

    let mut consumed = vec![false; changes.len()];
    let mut moves = Vec::new();
    for (d, c) in pairs {
        consumed[d] = true;
        consumed[c] = true;
        if let (
            Change::Delete { path: from, .. },
            Change::Create {
                path: to,
                size,
                mtime_ns,
            },
        ) = (&changes[d], &changes[c])
        {
            moves.push(Change::Move {
                from: from.clone(),
                to: to.clone(),
                kind: MoveKind::File {
                    size: *size,
                    mtime_ns: *mtime_ns,
                },
            });
        }
    }
    changes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !consumed[*i])
        .map(|(_, c)| c)
        .chain(moves)
        .collect()
}

fn collapse_folder_moves(
    changes: Vec<Change>,
    source: &Snapshot,
    replica: &Snapshot,
    case: CaseMode,
) -> Vec<Change> {
    let k = |r: &RelPath| match case {
        CaseMode::Sensitive => r.as_str().to_owned(),
        CaseMode::Insensitive => r.fold(),
    };
    let mut move_to: HashMap<String, String> = HashMap::new();
    let mut candidates: BTreeSet<(usize, RelPath, RelPath)> = BTreeSet::new();
    for c in &changes {
        let Change::Move {
            from,
            to,
            kind: MoveKind::File { .. },
        } = c
        else {
            continue;
        };
        if k(from) == k(to) {
            continue; // capital-letters-only rename
        }
        move_to.insert(k(from), k(to));
        let (mut x, mut y) = (from.parent(), to.parent());
        while let (Some(px), Some(py)) = (x, y) {
            if px.is_root() || py.is_root() {
                break;
            }
            candidates.insert((px.depth(), px.clone(), py.clone()));
            if px.name() != py.name() {
                break;
            }
            (x, y) = (px.parent(), py.parent());
        }
    }
    if candidates.is_empty() {
        return changes;
    }

    let src_keys: HashSet<String> = source.entries.iter().map(|e| k(&e.rel)).collect();
    let src_dirs: HashSet<String> = source
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Dir)
        .map(|e| k(&e.rel))
        .collect();
    let rep_keys: HashSet<String> = replica.entries.iter().map(|e| k(&e.rel)).collect();
    let is_rep_dir = |x: &RelPath| {
        replica
            .entries
            .binary_search_by(|e| e.rel.cmp(x))
            .is_ok_and(|i| replica.entries[i].kind == Kind::Dir)
    };

    let problem_keys: Vec<String> = replica.problems.iter().map(|p| k(&p.rel)).collect();
    // An unreadable entry at or under X must not be carried along by a rename.
    let has_problem = |x: &RelPath| {
        let kx = k(x);
        problem_keys.iter().any(|p| {
            *p == kx
                || p.strip_prefix(kx.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
    };

    let mut done: Vec<(RelPath, RelPath, u32, u64)> = Vec::new();
    for (_, x, y) in candidates {
        if x.is_within(&y) || y.is_within(&x) || !is_rep_dir(&x) || has_problem(&x) {
            continue;
        }
        if src_keys.contains(&k(&x)) || rep_keys.contains(&k(&y)) {
            continue;
        }
        if done.iter().any(|(dx, dy, ..)| {
            x.is_within(dx) || dx.is_within(&x) || y.is_within(dy) || dy.is_within(&y)
        }) {
            continue;
        }
        let prefix = format!("{x}/");
        let start = replica
            .entries
            .partition_point(|e| e.rel.as_str() < prefix.as_str());
        let under = replica.entries[start..]
            .iter()
            .take_while(|e| e.rel.as_str().starts_with(&prefix));
        let (mut ok, mut files, mut bytes) = (true, 0u32, 0u64);
        for e in under {
            let target = k(&y.join(e.rel.strip_dir(&x).expect("entry is under x")));
            ok = match e.kind {
                Kind::File if move_to.get(&k(&e.rel)) == Some(&target) => {
                    files += 1;
                    bytes += e.size;
                    true
                }
                Kind::Dir => src_dirs.contains(&target),
                _ => false,
            };
            if !ok {
                break;
            }
        }
        if ok && files > 0 {
            done.push((x, y, files, bytes));
        }
    }
    if done.is_empty() {
        return changes;
    }

    let in_x = |p: &RelPath| done.iter().any(|(x, ..)| p.is_within(x));
    let rep_dirs: HashSet<String> = replica
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Dir)
        .map(|e| k(&e.rel))
        .collect();
    // A MkDir under Y is covered only when it is one of X's folders being renamed.
    let covered_by_rename = |p: &RelPath| {
        done.iter().any(|(x, y, ..)| {
            p.is_within(y)
                && match p.strip_dir(y) {
                    None => true,
                    Some(rest) => rep_dirs.contains(&k(&x.join(rest))),
                }
        })
    };
    let mut out: Vec<Change> = changes
        .into_iter()
        .filter(|c| match c {
            Change::Move {
                from,
                kind: MoveKind::File { .. },
                ..
            } => !in_x(from),
            Change::RmDir { path } => !in_x(path),
            Change::MkDir { path } => !covered_by_rename(path),
            _ => true,
        })
        .collect();
    out.extend(done.into_iter().map(|(x, y, files, bytes)| Change::Move {
        from: x,
        to: y,
        kind: MoveKind::Dir { files, bytes },
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Change, MoveKind};
    use crate::testutil::{T0, ns, rel, snap};

    fn create(p: &str, size: u64, secs: i64) -> Change {
        Change::Create {
            path: rel(p),
            size,
            mtime_ns: ns(secs),
        }
    }
    fn delete(p: &str, size: u64, secs: i64) -> Change {
        Change::Delete {
            path: rel(p),
            size,
            mtime_ns: ns(secs),
        }
    }
    fn run(changes: Vec<Change>) -> Vec<Change> {
        detect_moves(changes, &snap(vec![]), &snap(vec![]), CaseMode::Sensitive)
    }

    #[test]
    fn unique_match_becomes_a_move() {
        let out = run(vec![
            delete("old/a.jpg", 5, T0),
            create("new/a.jpg", 5, T0 + 1),
        ]);
        assert_eq!(
            out,
            vec![Change::Move {
                from: rel("old/a.jpg"),
                to: rel("new/a.jpg"),
                kind: MoveKind::File {
                    size: 5,
                    mtime_ns: ns(T0 + 1)
                },
            }]
        );
    }

    #[test]
    fn mtime_outside_tolerance_is_not_a_move() {
        let out = run(vec![
            delete("old/a.jpg", 5, T0),
            create("new/a.jpg", 5, T0 + 3),
        ]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn different_name_or_size_is_not_a_move() {
        assert_eq!(
            run(vec![delete("a.jpg", 5, T0), create("b.jpg", 5, T0)]).len(),
            2
        );
        assert_eq!(
            run(vec![delete("x/a.jpg", 5, T0), create("y/a.jpg", 6, T0)]).len(),
            2
        );
    }

    #[test]
    fn ambiguous_matches_stay_delete_and_create() {
        let two_deletes = run(vec![
            delete("x/a", 5, T0),
            delete("y/a", 5, T0),
            create("z/a", 5, T0),
        ]);
        assert!(
            two_deletes
                .iter()
                .all(|c| !matches!(c, Change::Move { .. }))
        );
        let two_creates = run(vec![
            delete("x/a", 5, T0),
            create("y/a", 5, T0),
            create("z/a", 5, T0),
        ]);
        assert!(
            two_creates
                .iter()
                .all(|c| !matches!(c, Change::Move { .. }))
        );
    }

    #[test]
    fn other_changes_pass_through() {
        let up = Change::Update {
            path: rel("u"),
            size: 1,
            mtime_ns: 0,
            replica_newer: false,
        };
        let out = run(vec![up.clone(), Change::MkDir { path: rel("m") }]);
        assert_eq!(out, vec![up, Change::MkDir { path: rel("m") }]);
    }

    #[test]
    fn several_moves_come_out_in_input_order() {
        let out = run(vec![
            delete("x/a.jpg", 5, T0),
            create("y/a.jpg", 5, T0 + 1),
            delete("x/b.jpg", 10, T0),
            create("y/b.jpg", 10, T0 + 1),
        ]);
        assert_eq!(
            out,
            vec![
                Change::Move {
                    from: rel("x/a.jpg"),
                    to: rel("y/a.jpg"),
                    kind: MoveKind::File {
                        size: 5,
                        mtime_ns: ns(T0 + 1)
                    },
                },
                Change::Move {
                    from: rel("x/b.jpg"),
                    to: rel("y/b.jpg"),
                    kind: MoveKind::File {
                        size: 10,
                        mtime_ns: ns(T0 + 1)
                    },
                },
            ]
        );
    }

    use crate::diff::diff;
    use crate::model::Entry;
    use crate::testutil::{dir, file};

    fn pipeline(src: Vec<Entry>, rep: Vec<Entry>) -> Vec<Change> {
        let (s, r) = (snap(src), snap(rep));
        let mut out = detect_moves(
            diff(&s, &r, CaseMode::Sensitive),
            &s,
            &r,
            CaseMode::Sensitive,
        );
        out.sort_by(|a, b| a.path().cmp(b.path()));
        out
    }

    #[test]
    fn whole_folder_move_collapses_to_one_rename() {
        let out = pipeline(
            vec![
                dir("Old"),
                file("Old/keep.txt", 1, T0),
                dir("2025"),
                dir("2025/Trip"),
                dir("2025/Trip/raw"),
                file("2025/Trip/a.jpg", 10, T0),
                file("2025/Trip/b.jpg", 20, T0),
                file("2025/Trip/raw/c.nef", 30, T0),
            ],
            vec![
                dir("Old"),
                file("Old/keep.txt", 1, T0),
                dir("Old/Trip"),
                dir("Old/Trip/raw"),
                file("Old/Trip/a.jpg", 10, T0),
                file("Old/Trip/b.jpg", 20, T0),
                file("Old/Trip/raw/c.nef", 30, T0),
            ],
        );
        assert_eq!(
            out,
            vec![Change::Move {
                from: rel("Old/Trip"),
                to: rel("2025/Trip"),
                kind: MoveKind::Dir {
                    files: 3,
                    bytes: 60
                },
            }]
        );
    }

    #[test]
    fn collapses_at_the_highest_folder_that_fully_moved() {
        let out = pipeline(
            vec![dir("New"), dir("New/Trip"), file("New/Trip/a.jpg", 10, T0)],
            vec![dir("Old"), dir("Old/Trip"), file("Old/Trip/a.jpg", 10, T0)],
        );
        assert_eq!(
            out,
            vec![Change::Move {
                from: rel("Old"),
                to: rel("New"),
                kind: MoveKind::Dir {
                    files: 1,
                    bytes: 10
                },
            }]
        );
    }

    #[test]
    fn partly_moved_folder_stays_file_moves() {
        let out = pipeline(
            vec![dir("B"), file("B/a.jpg", 10, T0)],
            vec![dir("A"), file("A/a.jpg", 10, T0), file("A/left.txt", 5, T0)],
        );
        assert!(out.iter().any(|c| matches!(
            c,
            Change::Move {
                kind: MoveKind::File { .. },
                ..
            }
        )));
        assert!(out.iter().all(|c| !matches!(
            c,
            Change::Move {
                kind: MoveKind::Dir { .. },
                ..
            }
        )));
    }

    #[test]
    fn target_folder_already_on_replica_is_not_collapsed() {
        let out = pipeline(
            vec![
                dir("B"),
                file("B/a.jpg", 10, T0),
                file("B/other.txt", 1, T0),
            ],
            vec![
                dir("A"),
                file("A/a.jpg", 10, T0),
                dir("B"),
                file("B/other.txt", 1, T0),
            ],
        );
        assert!(out.iter().all(|c| !matches!(
            c,
            Change::Move {
                kind: MoveKind::Dir { .. },
                ..
            }
        )));
    }

    #[test]
    fn moving_into_own_subfolder_is_not_collapsed() {
        let out = pipeline(
            vec![dir("a"), dir("a/b"), file("a/b/x", 1, T0)],
            vec![dir("a"), file("a/x", 1, T0)],
        );
        assert!(out.iter().all(|c| !matches!(
            c,
            Change::Move {
                kind: MoveKind::Dir { .. },
                ..
            }
        )));
    }

    fn dir_moves(out: &[Change]) -> Vec<&Change> {
        out.iter()
            .filter(|c| {
                matches!(
                    c,
                    Change::Move {
                        kind: MoveKind::Dir { .. },
                        ..
                    }
                )
            })
            .collect()
    }

    #[test]
    fn uncovered_empty_source_folder_keeps_its_mkdir() {
        let out = pipeline(
            vec![
                dir("2025"),
                dir("2025/Trip"),
                dir("2025/Trip/empty"),
                file("2025/Trip/a.jpg", 10, T0),
            ],
            vec![dir("Old"), dir("Old/Trip"), file("Old/Trip/a.jpg", 10, T0)],
        );
        assert_eq!(
            out,
            vec![
                Change::Move {
                    from: rel("Old"),
                    to: rel("2025"),
                    kind: MoveKind::Dir {
                        files: 1,
                        bytes: 10
                    },
                },
                Change::MkDir {
                    path: rel("2025/Trip/empty")
                },
            ]
        );
    }

    #[test]
    fn nested_destinations_are_not_both_collapsed() {
        let out = pipeline(
            vec![
                dir("c"),
                dir("c/d"),
                dir("c/d/e"),
                file("c/d/e/f", 1, T0),
                file("c/d/g", 2, T0),
            ],
            vec![dir("a"), file("a/f", 1, T0), dir("p"), file("p/g", 2, T0)],
        );
        let dirs = dir_moves(&out);
        assert!(dirs.len() <= 1);
        for a in &dirs {
            for b in &dirs {
                if let (Change::Move { to: ta, .. }, Change::Move { to: tb, .. }) = (a, b) {
                    assert!(ta == tb || !(ta.is_within(tb) || tb.is_within(ta)));
                }
            }
        }
        let file_moves = out
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    Change::Move {
                        kind: MoveKind::File { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(file_moves + dirs.len(), 2);
    }

    #[test]
    fn insensitive_mode_collapses_with_mixed_case() {
        let (s, r) = (
            snap(vec![
                dir("New"),
                dir("New/Trip"),
                file("New/Trip/a.jpg", 10, T0),
            ]),
            snap(vec![
                dir("old"),
                dir("old/Trip"),
                file("old/Trip/a.jpg", 10, T0),
            ]),
        );
        let out = detect_moves(
            diff(&s, &r, CaseMode::Insensitive),
            &s,
            &r,
            CaseMode::Insensitive,
        );
        assert_eq!(
            out,
            vec![Change::Move {
                from: rel("old"),
                to: rel("New"),
                kind: MoveKind::Dir {
                    files: 1,
                    bytes: 10
                },
            }]
        );
    }

    #[test]
    fn link_under_folder_rejects_collapse() {
        let out = pipeline(
            vec![dir("B"), file("B/a.jpg", 10, T0)],
            vec![
                dir("A"),
                file("A/a.jpg", 10, T0),
                crate::testutil::link("A/l"),
            ],
        );
        assert!(dir_moves(&out).is_empty());
    }

    #[test]
    fn folder_still_on_source_rejects_collapse() {
        let out = pipeline(
            vec![
                dir("A"),
                file("A/x", 1, T0),
                dir("B"),
                file("B/a.jpg", 10, T0),
            ],
            vec![dir("A"), file("A/a.jpg", 10, T0)],
        );
        assert!(dir_moves(&out).is_empty());
    }
    #[test]
    fn unreadable_replica_subfolder_rejects_collapse() {
        let s = snap(vec![
            dir("New"),
            file("New/a.jpg", 10, T0),
            dir("New/locked"),
            file("New/locked/b.jpg", 20, T0),
        ]);
        let mut r = snap(vec![
            dir("Old"),
            file("Old/a.jpg", 10, T0),
            dir("Old/locked"),
        ]);
        r.problems.push(crate::model::Problem {
            rel: rel("Old/locked"),
            reason: "Permission denied".into(),
        });
        let out = detect_moves(
            diff(&s, &r, CaseMode::Sensitive),
            &s,
            &r,
            CaseMode::Sensitive,
        );
        assert!(dir_moves(&out).is_empty(), "{out:?}");
        // a problem on a file (no entry at all) under the folder rejects it too
        let mut r = snap(vec![dir("Old"), file("Old/a.jpg", 10, T0)]);
        r.problems.push(crate::model::Problem {
            rel: rel("Old/x.bin"),
            reason: "Permission denied".into(),
        });
        let s = snap(vec![dir("New"), file("New/a.jpg", 10, T0)]);
        let out = detect_moves(
            diff(&s, &r, CaseMode::Sensitive),
            &s,
            &r,
            CaseMode::Sensitive,
        );
        assert!(dir_moves(&out).is_empty(), "{out:?}");
    }
}
