//! The ordered, numbered list of changes the user approves.

use crate::diff::CaseMode;
use crate::model::{Change, MoveKind, RelPath};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ChangeId(pub u32);

#[derive(Clone, Debug, Serialize)]
pub struct Planned {
    pub id: ChangeId,
    pub change: Change,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Totals {
    pub creates: usize,
    pub updates: usize,
    pub moves: usize,
    pub deletes: usize,
    /// MkDir + RmDir.
    pub folders: usize,
    pub skipped: usize,
    pub bytes_to_copy: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Plan {
    pub changes: Vec<Planned>,
    pub totals: Totals,
}

impl Plan {
    /// True when nothing would be applied ("Already in sync").
    pub fn is_empty(&self) -> bool {
        self.changes
            .iter()
            .all(|p| matches!(p.change, Change::Skipped { .. }))
    }

    /// The default selection: every change except `Skipped`.
    pub fn actionable_ids(&self) -> HashSet<ChangeId> {
        self.changes
            .iter()
            .filter(|p| !matches!(p.change, Change::Skipped { .. }))
            .map(|p| p.id)
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("two changes would write {0}")]
    DuplicateTarget(RelPath),
}

fn rank(c: &Change) -> u8 {
    match c {
        Change::Move {
            kind: MoveKind::Dir { .. },
            ..
        } => 0,
        Change::Move { .. } => 1,
        Change::MkDir { .. } => 2,
        Change::Create { .. } | Change::Update { .. } => 3,
        Change::Delete { .. } => 4,
        Change::RmDir { .. } => 5,
        Change::Skipped { .. } => 6,
    }
}

fn order_key(c: &Change) -> (i64, &str) {
    match c {
        Change::RmDir { path } => (-(path.depth() as i64), path.as_str()),
        Change::MkDir { path } => (path.depth() as i64, path.as_str()),
        Change::Move { from, .. } => (0, from.as_str()),
        other => (0, other.path().as_str()),
    }
}

pub fn build_plan(mut changes: Vec<Change>, case: CaseMode) -> Result<Plan, PlanError> {
    let mut targets = HashSet::new();
    for c in &changes {
        let target = match c {
            Change::Create { path, .. } | Change::MkDir { path } => path,
            Change::Move { to, .. } => to,
            _ => continue,
        };
        let k = match case {
            CaseMode::Sensitive => target.as_str().to_owned(),
            CaseMode::Insensitive => target.fold(),
        };
        if !targets.insert(k) {
            return Err(PlanError::DuplicateTarget(target.clone()));
        }
    }
    changes.sort_by(|a, b| {
        rank(a)
            .cmp(&rank(b))
            .then_with(|| order_key(a).cmp(&order_key(b)))
    });
    let totals = totals(changes.iter());
    let changes = changes
        .into_iter()
        .enumerate()
        .map(|(i, change)| Planned {
            id: ChangeId(i as u32),
            change,
        })
        .collect();
    Ok(Plan { changes, totals })
}

pub fn totals<'a>(changes: impl Iterator<Item = &'a Change>) -> Totals {
    let mut t = Totals::default();
    for c in changes {
        match c {
            Change::Create { .. } => t.creates += 1,
            Change::Update { .. } => t.updates += 1,
            Change::Move { .. } => t.moves += 1,
            Change::Delete { .. } => t.deletes += 1,
            Change::MkDir { .. } | Change::RmDir { .. } => t.folders += 1,
            Change::Skipped { .. } => t.skipped += 1,
        }
        t.bytes_to_copy += c.bytes_to_copy();
    }
    t
}

pub fn selected_totals(plan: &Plan, approved: &HashSet<ChangeId>) -> Totals {
    totals(
        plan.changes
            .iter()
            .filter(|p| approved.contains(&p.id))
            .map(|p| &p.change),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GuardWarning {
    pub affected: u64,
    pub replica_files: u64,
}

/// On a pair's first sync: warn when the approved plan deletes or replaces
/// more than half of the replica's files (the replica may be the wrong folder).
pub fn first_sync_guard(
    plan: &Plan,
    approved: &HashSet<ChangeId>,
    replica_files: u64,
) -> Option<GuardWarning> {
    let affected = plan
        .changes
        .iter()
        .filter(|p| {
            approved.contains(&p.id)
                && matches!(p.change, Change::Delete { .. } | Change::Update { .. })
        })
        .count() as u64;
    (replica_files > 0 && affected * 2 > replica_files).then_some(GuardWarning {
        affected,
        replica_files,
    })
}

/// How many bytes short the replica drive is, if any. Trashing frees nothing.
pub fn space_shortfall(bytes_needed: u64, free: u64) -> Option<u64> {
    (bytes_needed > free).then(|| bytes_needed - free)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MoveKind;
    use crate::testutil::rel;

    fn create(p: &str, size: u64) -> Change {
        Change::Create {
            path: rel(p),
            size,
            mtime_ns: 0,
        }
    }
    fn delete(p: &str) -> Change {
        Change::Delete {
            path: rel(p),
            size: 1,
            mtime_ns: 0,
        }
    }

    #[test]
    fn orders_by_apply_phase_and_numbers_ids() {
        let plan = build_plan(
            vec![
                Change::RmDir { path: rel("a") },
                delete("z"),
                Change::Skipped {
                    path: rel("s"),
                    reason: "link (not followed)".into(),
                },
                create("b", 1),
                Change::MkDir { path: rel("m/n") },
                Change::RmDir { path: rel("a/b") },
                Change::Move {
                    from: rel("f1"),
                    to: rel("f2"),
                    kind: MoveKind::File {
                        size: 1,
                        mtime_ns: 0,
                    },
                },
                Change::Move {
                    from: rel("d1"),
                    to: rel("d2"),
                    kind: MoveKind::Dir { files: 2, bytes: 2 },
                },
                Change::Update {
                    path: rel("a0"),
                    size: 1,
                    mtime_ns: 0,
                    replica_newer: false,
                },
                Change::MkDir { path: rel("m") },
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let order: Vec<String> = plan
            .changes
            .iter()
            .map(|p| p.change.path().to_string())
            .collect();
        assert_eq!(
            order,
            vec!["d2", "f2", "m", "m/n", "a0", "b", "z", "a/b", "a", "s"]
        );
        let ids: Vec<u32> = plan.changes.iter().map(|p| p.id.0).collect();
        assert_eq!(ids, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn totals_count_kinds_and_bytes() {
        let plan = build_plan(
            vec![
                create("a", 10),
                create("b", 5),
                delete("c"),
                Change::MkDir { path: rel("d") },
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        assert_eq!(
            plan.totals,
            Totals {
                creates: 2,
                deletes: 1,
                folders: 1,
                bytes_to_copy: 15,
                ..Totals::default()
            }
        );
        let only_a: HashSet<ChangeId> = plan
            .changes
            .iter()
            .filter(|p| p.change.path().as_str() == "a")
            .map(|p| p.id)
            .collect();
        assert_eq!(selected_totals(&plan, &only_a).bytes_to_copy, 10);
        assert_eq!(plan.actionable_ids().len(), 4);
    }

    #[test]
    fn duplicate_targets_are_rejected_respecting_case_mode() {
        assert!(build_plan(vec![create("a", 1), create("a", 1)], CaseMode::Sensitive).is_err());
        assert!(build_plan(vec![create("A", 1), create("a", 1)], CaseMode::Sensitive).is_ok());
        assert!(matches!(
            build_plan(vec![create("A", 1), create("a", 1)], CaseMode::Insensitive),
            Err(PlanError::DuplicateTarget(_))
        ));
    }

    #[test]
    fn wrong_folder_guard_triggers_above_half() {
        let changes: Vec<Change> = (0..6).map(|i| delete(&format!("f{i}"))).collect();
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let all = plan.actionable_ids();
        assert_eq!(
            first_sync_guard(&plan, &all, 10),
            Some(GuardWarning {
                affected: 6,
                replica_files: 10
            })
        );
        assert_eq!(first_sync_guard(&plan, &all, 12), None);
        assert_eq!(first_sync_guard(&plan, &all, 0), None);
        assert_eq!(first_sync_guard(&plan, &HashSet::new(), 10), None);
    }

    #[test]
    fn space_shortfall_reports_missing_bytes() {
        assert_eq!(space_shortfall(100, 40), Some(60));
        assert_eq!(space_shortfall(100, 100), None);
    }
}
