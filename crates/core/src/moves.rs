//! Turn Delete+Create pairs into renames, and whole moved folders into one rename.

use crate::model::CaseMode;
use crate::model::{Change, Kind, MoveKind, RelPath, Snapshot, fold, times_match};
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

pub fn detect_moves(
    changes: Vec<Change>,
    source: &Snapshot,
    replica: &Snapshot,
    case: CaseMode,
) -> Vec<Change> {
    let changes = pair_file_moves(changes, case);
    collapse_folder_moves(changes, source, replica, case)
}

/// One create or delete in a pairing bucket.
struct Candidate<'a> {
    mtime_ns: i64,
    index: usize,
    path: &'a RelPath,
}

/// On a case-insensitive replica `A.jpg` and `a.jpg` are one name, so a file
/// moved and renamed only in capital letters still pairs.
fn name_key(p: &RelPath, case: CaseMode) -> Cow<'_, str> {
    match case {
        CaseMode::Sensitive => Cow::Borrowed(p.name()),
        CaseMode::Insensitive => Cow::Owned(fold(p.name())),
    }
}

fn pair_file_moves(changes: Vec<Change>, case: CaseMode) -> Vec<Change> {
    let moves: Vec<(usize, usize, Change)> = {
        type Buckets<'a> = HashMap<(Cow<'a, str>, u64), Vec<Candidate<'a>>>;
        let (mut creates, mut deletes): (Buckets, Buckets) = Default::default();
        for (index, c) in changes.iter().enumerate() {
            let (bucket, path, size, mtime_ns) = match c {
                Change::Create {
                    path,
                    size,
                    mtime_ns,
                } => (&mut creates, path, size, mtime_ns),
                Change::Delete {
                    path,
                    size,
                    mtime_ns,
                } => (&mut deletes, path, size, mtime_ns),
                _ => continue,
            };
            bucket
                .entry((name_key(path, case), *size))
                .or_default()
                .push(Candidate {
                    mtime_ns: *mtime_ns,
                    index,
                    path,
                });
        }
        let window = |sorted: &[Candidate<'_>], m: i64| {
            let lo = sorted.partition_point(|c| c.mtime_ns < m && !times_match(c.mtime_ns, m));
            let hi = sorted.partition_point(|c| c.mtime_ns <= m || times_match(c.mtime_ns, m));
            lo..hi
        };
        let mut moves = Vec::new();
        for (key, dels) in deletes {
            let Some(mut crs) = creates.remove(&key) else {
                continue;
            };
            // Each bucket sorted by mtime: the changes within the 2 s window of
            // an mtime are one contiguous run, found by two binary searches.
            let mut dels = dels;
            dels.sort_by_key(|c| (c.mtime_ns, c.index));
            crs.sort_by_key(|c| (c.mtime_ns, c.index));
            for d in &dels {
                // Pairs only when exactly one create is close to this delete and
                // exactly one delete (this one) is close to that create.
                let near = window(&crs, d.mtime_ns);
                if near.len() != 1 {
                    continue;
                }
                let c = &crs[near.start];
                if window(&dels, c.mtime_ns).len() == 1 {
                    moves.push((
                        d.index,
                        c.index,
                        Change::Move {
                            from: d.path.clone(),
                            to: c.path.clone(),
                            kind: MoveKind::File {
                                size: key.1,
                                mtime_ns: c.mtime_ns,
                            },
                        },
                    ));
                }
            }
        }
        moves.sort_unstable_by_key(|(d, c, _)| (*d, *c));
        moves
    };

    let mut consumed = vec![false; changes.len()];
    for (d, c, _) in &moves {
        consumed[*d] = true;
        consumed[*c] = true;
    }
    changes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !consumed[*i])
        .map(|(_, c)| c)
        .chain(moves.into_iter().map(|(_, _, m)| m))
        .collect()
}

fn collapse_folder_moves(
    changes: Vec<Change>,
    source: &Snapshot,
    replica: &Snapshot,
    case: CaseMode,
) -> Vec<Change> {
    let k = |r: &RelPath| case.key(r);
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
        if from.name() != to.name() {
            // Renamed in capital letters as it moved: a folder rename would
            // carry the old spelling along, so it stays a move of its own.
            continue;
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

    // An unreadable entry, or one a skip rule hides (leftovers included), at or
    // under X must not be carried along by a rename: the skipped entry would
    // land where its rule no longer matches it.
    let blocked = PathTree(
        replica
            .problems
            .iter()
            .map(|p| &p.rel)
            .chain(&replica.unlisted)
            .chain(replica.leftovers.iter().map(|e| &e.rel))
            .map(k)
            .collect(),
    );
    let has_problem = |x: &RelPath| {
        let kx = k(x);
        blocked.0.contains(&kx) || blocked.has_below(&kx)
    };

    // The files and bytes under replica folder X when every entry there goes
    // to the same place under Y (files moved there, folders on the source);
    // `None` when one doesn't, or when the count doesn't fit the move's fields.
    let moves_whole = |x: &RelPath, y: &RelPath| -> Option<(u32, u64)> {
        let prefix = format!("{x}/");
        let start = replica
            .entries
            .partition_point(|e| e.rel.as_str() < prefix.as_str());
        let under = replica.entries[start..]
            .iter()
            .take_while(|e| e.rel.as_str().starts_with(&prefix));
        let (mut files, mut bytes) = (0u32, 0u64);
        for e in under {
            let target = k(&y.join(e.rel.strip_dir(x)?));
            match e.kind {
                Kind::File if move_to.get(&k(&e.rel)) == Some(&target) => {
                    files = files.checked_add(1)?;
                    bytes = bytes.checked_add(e.size)?;
                }
                Kind::Dir if src_dirs.contains(&target) => {}
                _ => return None,
            }
        }
        (files > 0).then_some((files, bytes))
    };

    let mut done: Vec<(RelPath, RelPath, u32, u64)> = Vec::new();
    // The accepted X and Y folders, for "overlaps an accepted move" lookups.
    let (mut done_x, mut done_y) = (PathTree::default(), PathTree::default());
    for (_, x, y) in candidates {
        if x.is_within(&y) || y.is_within(&x) || !is_rep_dir(&x) || has_problem(&x) {
            continue;
        }
        if src_keys.contains(&k(&x)) || rep_keys.contains(&k(&y)) {
            continue;
        }
        if done_x.overlaps(x.as_str()) || done_y.overlaps(y.as_str()) {
            continue;
        }
        if let Some((files, bytes)) = moves_whole(&x, &y) {
            done_x.0.insert(x.as_str().to_owned());
            done_y.0.insert(y.as_str().to_owned());
            done.push((x, y, files, bytes));
        }
    }
    if done.is_empty() {
        return changes;
    }

    let in_x = |p: &RelPath| done_x.at_or_above(p.as_str()).is_some();
    let x_of_y: HashMap<&str, &RelPath> = done.iter().map(|(x, y, ..)| (y.as_str(), x)).collect();
    let rep_dirs: HashSet<String> = replica
        .entries
        .iter()
        .filter(|e| e.kind == Kind::Dir)
        .map(|e| k(&e.rel))
        .collect();
    // A MkDir under Y is covered only when it is one of X's folders being renamed.
    // Accepted Y folders never overlap, so at most one holds `p`.
    let covered_by_rename = |p: &RelPath| match done_y.at_or_above(p.as_str()) {
        None => false,
        Some(y) => match p.as_str().strip_prefix(y).and_then(|r| r.strip_prefix('/')) {
            None => true,
            Some(rest) => rep_dirs.contains(&k(&x_of_y[y].join(rest))),
        },
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

/// A set of folder paths answering "is a member at, above or below this path"
/// in O(depth · log n), instead of a scan of every member.
#[derive(Default)]
struct PathTree(BTreeSet<String>);

impl PathTree {
    /// The member that is `p` or holds it, if any.
    fn at_or_above<'a>(&'a self, p: &'a str) -> Option<&'a str> {
        let mut cur = p;
        loop {
            if let Some(m) = self.0.get(cur) {
                return Some(m.as_str());
            }
            cur = &cur[..cur.rfind('/')?];
        }
    }

    /// Whether a member lies strictly below `p`.
    fn has_below(&self, p: &str) -> bool {
        let prefix = format!("{p}/");
        self.0
            .range(prefix.clone()..)
            .next()
            .is_some_and(|m| m.starts_with(&prefix))
    }

    /// Whether a member is `p`, holds it, or lies below it.
    fn overlaps(&self, p: &str) -> bool {
        self.at_or_above(p).is_some() || self.has_below(p)
    }
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
    fn a_name_differing_only_in_case_pairs_on_an_insensitive_replica_only() {
        let changes = || vec![delete("old/A.jpg", 5, T0), create("new/a.jpg", 5, T0)];
        let insensitive = detect_moves(
            changes(),
            &snap(vec![]),
            &snap(vec![]),
            CaseMode::Insensitive,
        );
        assert_eq!(
            insensitive,
            vec![Change::Move {
                from: rel("old/A.jpg"),
                to: rel("new/a.jpg"),
                kind: MoveKind::File {
                    size: 5,
                    mtime_ns: ns(T0)
                },
            }]
        );
        assert_eq!(run(changes()), changes());
    }

    /// Renaming `b` to `A` would land the file at `A/a`, not `A/A`.
    #[test]
    fn a_file_renamed_in_case_as_it_moved_is_not_carried_by_a_folder_rename() {
        let out = detect_moves(
            vec![delete("b/a", 5, T0), create("A/A", 5, T0)],
            &snap(vec![dir("A"), file("A/A", 5, T0)]),
            &snap(vec![dir("b"), file("b/a", 5, T0)]),
            CaseMode::Insensitive,
        );
        assert_eq!(
            out,
            vec![Change::Move {
                from: rel("b/a"),
                to: rel("A/A"),
                kind: MoveKind::File {
                    size: 5,
                    mtime_ns: ns(T0)
                },
            }]
        );
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

    /// The output has no move and still holds every original change.
    fn assert_untouched(input: Vec<Change>) {
        let mut out = run(input.clone());
        assert!(
            out.iter().all(|c| !matches!(c, Change::Move { .. })),
            "{out:?}"
        );
        let mut want = input;
        out.sort_by(|a, b| a.path().cmp(b.path()));
        want.sort_by(|a, b| a.path().cmp(b.path()));
        assert_eq!(out, want);
    }

    #[test]
    fn ambiguous_matches_stay_delete_and_create() {
        assert_untouched(vec![
            delete("x/a", 5, T0),
            delete("y/a", 5, T0),
            create("z/a", 5, T0),
        ]);
        assert_untouched(vec![
            delete("x/a", 5, T0),
            create("y/a", 5, T0),
            create("z/a", 5, T0),
        ]);
    }

    #[test]
    fn mtimes_exactly_the_tolerance_apart_pair_and_one_nanosecond_more_does_not() {
        let at = |ns_apart: i64| {
            run(vec![
                Change::Delete {
                    path: rel("old/a.jpg"),
                    size: 5,
                    mtime_ns: ns(T0),
                },
                Change::Create {
                    path: rel("new/a.jpg"),
                    size: 5,
                    mtime_ns: ns(T0) + ns_apart,
                },
            ])
        };
        let tolerance = ns(2);
        for apart in [tolerance, -tolerance] {
            let out = at(apart);
            assert_eq!(out.len(), 1, "{apart}: {out:?}");
            assert!(matches!(out[0], Change::Move { .. }), "{out:?}");
        }
        for apart in [tolerance + 1, -tolerance - 1] {
            assert_untouched(vec![
                delete("old/a.jpg", 5, T0),
                Change::Create {
                    path: rel("new/a.jpg"),
                    size: 5,
                    mtime_ns: ns(T0) + apart,
                },
            ]);
        }
    }

    #[test]
    fn other_changes_pass_through() {
        let up = Change::Update {
            path: rel("u"),
            size: 1,
            mtime_ns: 0,
            replica_size: 0,
            replica_mtime_ns: 0,
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

    /// `n` albums of four photos each, every album renamed on the source.
    fn renamed_albums(n: usize) -> (Vec<Entry>, Vec<Entry>) {
        let (mut src, mut rep) = (Vec::new(), Vec::new());
        for i in 0..n {
            let secs = T0 + 10 * i as i64;
            src.push(dir(&format!("trip{i}")));
            rep.push(dir(&format!("album{i}")));
            for f in ["a", "b", "c", "d"] {
                src.push(file(&format!("trip{i}/{f}.jpg"), 4, secs));
                rep.push(file(&format!("album{i}/{f}.jpg"), 4, secs));
            }
        }
        (src, rep)
    }

    #[test]
    fn five_thousand_renamed_folders_collapse_quickly() {
        let (src, rep) = renamed_albums(5_000);
        let t = std::time::Instant::now();
        let out = pipeline(src, rep);
        let elapsed = t.elapsed();
        let dir_moves = out
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    Change::Move {
                        kind: MoveKind::Dir { files: 4, .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!((dir_moves, out.len()), (5_000, 5_000));
        assert!(elapsed.as_secs_f64() < 1.0, "took {elapsed:?}");
    }

    #[test]
    fn same_name_files_pair_only_when_their_times_are_unambiguous() {
        // Many `cover.jpg` of one size: each pairs with the one create within
        // 2 s, except where two deletes (or two creates) share a window.
        let mut changes = Vec::new();
        for i in 0..200 {
            let secs = T0 + 10 * i;
            changes.push(delete(&format!("old{i}/cover.jpg"), 7, secs));
            changes.push(create(&format!("new{i}/cover.jpg"), 7, secs + 1));
        }
        changes.push(delete("dupe/cover.jpg", 7, T0 + 10 * 50 + 2));
        let out = detect_moves(changes, &snap(vec![]), &snap(vec![]), CaseMode::Sensitive);
        let moves = out
            .iter()
            .filter(|c| matches!(c, Change::Move { .. }))
            .count();
        assert_eq!(moves, 199);
        assert!(out.iter().any(
            |c| matches!(c, Change::Create { path, .. } if path.as_str() == "new50/cover.jpg")
        ));
    }

    #[test]
    fn folder_holding_a_skipped_entry_is_not_collapsed() {
        let s = snap(vec![
            dir("New"),
            file("New/a.jpg", 10, T0),
            file("New/b.jpg", 20, T0),
        ]);
        let mut r = snap(vec![
            dir("Old"),
            file("Old/a.jpg", 10, T0),
            file("Old/b.jpg", 20, T0),
        ]);
        r.unlisted = vec![rel("Old/cache/x.bin")];
        let out = detect_moves(
            diff(&s, &r, CaseMode::Sensitive),
            &s,
            &r,
            CaseMode::Sensitive,
        );
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
        assert_eq!(file_moves, 2, "{out:?}");
        assert!(
            !out.iter().any(|c| matches!(
                c,
                Change::Move {
                    kind: MoveKind::Dir { .. },
                    ..
                }
            )),
            "{out:?}"
        );
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
            kind: crate::model::ProblemKind::Unreadable,
            detail: "Permission denied".into(),
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
            kind: crate::model::ProblemKind::Unreadable,
            detail: "Permission denied".into(),
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
