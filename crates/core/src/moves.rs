//! Turn Delete+Create pairs into renames, and whole moved folders into one rename.

use crate::diff::CaseMode;
use crate::model::{Change, MTIME_TOLERANCE_NS, MoveKind, Snapshot};
use std::collections::HashMap;

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
    _source: &Snapshot,
    _replica: &Snapshot,
    _case: CaseMode,
) -> Vec<Change> {
    changes // Task 6
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
}
