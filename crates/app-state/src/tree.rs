//! The plan as a folder tree for the Preview: children of one folder at a
//! time, per-folder counts and tick states. The plan stays in Rust; the UI
//! never holds more than the rows it shows.

use replica_sync_core::model::{Change, RelPath};
use replica_sync_core::plan::{ChangeId, Plan};
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Tick {
    On,
    Off,
    Mixed,
    /// Nothing here can be applied (only skipped changes).
    None,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub create: u32,
    pub update: u32,
    pub delete: u32,
    pub moves: u32,
    pub folders: u32,
    pub skipped: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowChange {
    pub id: ChangeId,
    pub change: Change,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeView {
    pub name: String,
    pub path: RelPath,
    pub is_folder: bool,
    pub changes: Vec<RowChange>,
    pub counts: Counts,
    pub bytes_to_copy: u64,
    pub tick: Tick,
}

struct Row {
    key: String,
    idx: usize,
}

pub struct PlanTree {
    rows: Vec<Row>,
}

fn key_of(p: &RelPath) -> String {
    p.as_str().replace('/', "\0")
}

impl PlanTree {
    pub fn new(plan: &Plan) -> PlanTree {
        let mut rows: Vec<Row> = plan
            .changes
            .iter()
            .enumerate()
            .map(|(idx, p)| Row {
                key: key_of(p.change.path()),
                idx,
            })
            .collect();
        rows.sort_by(|a, b| a.key.cmp(&b.key));
        PlanTree { rows }
    }

    /// Rows at `folder` or inside it, as an index range (the whole tree for the root).
    fn range(&self, folder: &RelPath) -> (usize, usize) {
        if folder.is_root() {
            return (0, self.rows.len());
        }
        let k = key_of(folder);
        let inside = format!("{k}\0");
        let lo = self.rows.partition_point(|r| r.key < k);
        let len = self.rows[lo..]
            .iter()
            .take_while(|r| r.key == k || r.key.starts_with(&inside))
            .count();
        (lo, lo + len)
    }

    fn actionable(plan: &Plan, idx: usize) -> bool {
        !matches!(plan.changes[idx].change, Change::Skipped { .. })
    }

    pub fn children(
        &self,
        plan: &Plan,
        folder: &RelPath,
        sel: &HashSet<ChangeId>,
    ) -> Vec<NodeView> {
        let (lo, hi) = self.range(folder);
        let skip = if folder.is_root() {
            0
        } else {
            folder.as_str().len() + 1
        };
        let mut out = Vec::new();
        let mut i = lo;
        while i < hi {
            let path = plan.changes[self.rows[i].idx].change.path();
            if !folder.is_root() && path == folder {
                i += 1; // the folder's own change is shown on its parent's row
                continue;
            }
            let name = path.as_str()[skip..]
                .split('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            let child = folder.join(&name);
            let (clo, chi) = self.range(&child);
            out.push(self.node(plan, &child, name, clo, chi, sel));
            i = chi.max(i + 1);
        }
        out
    }

    fn node(
        &self,
        plan: &Plan,
        path: &RelPath,
        name: String,
        lo: usize,
        hi: usize,
        sel: &HashSet<ChangeId>,
    ) -> NodeView {
        let mut counts = Counts::default();
        let (mut bytes, mut actionable, mut selected, mut is_folder) =
            (0u64, 0usize, 0usize, false);
        let mut changes = Vec::new();
        for r in &self.rows[lo..hi] {
            let p = &plan.changes[r.idx];
            match &p.change {
                Change::Create { .. } => counts.create += 1,
                Change::Update { .. } => counts.update += 1,
                Change::Delete { .. } => counts.delete += 1,
                Change::Move { .. } => counts.moves += 1,
                Change::MkDir { .. } | Change::RmDir { .. } => counts.folders += 1,
                Change::Skipped { .. } => counts.skipped += 1,
            }
            bytes += p.change.bytes_to_copy();
            if Self::actionable(plan, r.idx) {
                actionable += 1;
                selected += usize::from(sel.contains(&p.id));
            }
            if p.change.path() == path {
                changes.push(RowChange {
                    id: p.id,
                    change: p.change.clone(),
                });
            } else {
                is_folder = true;
            }
        }
        let tick = match (actionable, selected) {
            (0, _) => Tick::None,
            (_, 0) => Tick::Off,
            (a, s) if a == s => Tick::On,
            _ => Tick::Mixed,
        };
        NodeView {
            name,
            path: path.clone(),
            is_folder,
            changes,
            counts,
            bytes_to_copy: bytes,
            tick,
        }
    }

    pub fn toggle(&self, plan: &Plan, path: &RelPath, sel: &mut HashSet<ChangeId>) {
        let (lo, hi) = self.range(path);
        let ids: Vec<ChangeId> = self.rows[lo..hi]
            .iter()
            .filter(|r| Self::actionable(plan, r.idx))
            .map(|r| plan.changes[r.idx].id)
            .collect();
        if ids.iter().all(|id| sel.contains(id)) {
            for id in &ids {
                sel.remove(id);
            }
        } else {
            sel.extend(ids);
        }
    }

    pub fn folders_with_deletes(&self, plan: &Plan) -> Vec<RelPath> {
        let mut out = BTreeSet::new();
        for p in &plan.changes {
            if let Change::Delete { path, .. } = &p.change {
                let mut cur = path.parent();
                while let Some(dir) = cur {
                    if dir.is_root() {
                        break;
                    }
                    cur = dir.parent();
                    out.insert(dir);
                }
            }
        }
        out.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use replica_sync_core::diff::CaseMode;
    use replica_sync_core::model::MoveKind;
    use replica_sync_core::plan::build_plan;
    use replica_sync_core::reason::SkipReason;

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }
    fn create(p: &str, size: u64) -> Change {
        Change::Create {
            path: rel(p),
            size,
            mtime_ns: 0,
        }
    }

    fn fixture() -> (Plan, PlanTree) {
        let plan = build_plan(
            vec![
                create("a/b/1.txt", 10),
                create("a/b/2.txt", 20),
                Change::Delete {
                    path: rel("a/old.txt"),
                    size: 1,
                    mtime_ns: 0,
                },
                Change::Move {
                    from: rel("Old"),
                    to: rel("New"),
                    kind: MoveKind::Dir { files: 3, bytes: 9 },
                },
                Change::Update {
                    path: rel("top.txt"),
                    size: 5,
                    mtime_ns: 0,
                    replica_newer: false,
                },
                Change::Skipped {
                    path: rel("lnk/x"),
                    reason: SkipReason::Link,
                },
                create("a-c/x.txt", 1),
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        (plan, tree)
    }

    fn names(nodes: &[NodeView]) -> Vec<&str> {
        nodes.iter().map(|n| n.name.as_str()).collect()
    }

    #[test]
    fn root_children_group_by_first_component() {
        let (plan, tree) = fixture();
        let sel = plan.actionable_ids();
        let root = tree.children(&plan, &RelPath::root(), &sel);
        // byte order of the \0-separated keys: uppercase first, "a" before "a-c"
        assert_eq!(names(&root), vec!["New", "a", "a-c", "lnk", "top.txt"]);
        let a = root.iter().find(|n| n.name == "a").unwrap();
        assert!(a.is_folder);
        assert_eq!(
            a.counts,
            Counts {
                create: 2,
                delete: 1,
                ..Counts::default()
            }
        );
        assert_eq!(a.bytes_to_copy, 30);
        assert_eq!(a.tick, Tick::On);
        let new = root.iter().find(|n| n.name == "New").unwrap();
        assert!(!new.is_folder && new.changes.len() == 1);
        let lnk = root.iter().find(|n| n.name == "lnk").unwrap();
        assert_eq!(lnk.tick, Tick::None);
    }

    #[test]
    fn sibling_with_a_shared_prefix_is_not_inside_the_folder() {
        let (plan, tree) = fixture();
        let sel = plan.actionable_ids();
        let a = tree.children(&plan, &rel("a"), &sel);
        assert_eq!(names(&a), vec!["b", "old.txt"]);
    }

    #[test]
    fn toggling_a_folder_then_a_file_gives_off_then_mixed() {
        let (plan, tree) = fixture();
        let mut sel = plan.actionable_ids();
        tree.toggle(&plan, &rel("a"), &mut sel);
        let tick_a = |sel: &HashSet<ChangeId>| {
            tree.children(&plan, &RelPath::root(), sel)
                .into_iter()
                .find(|n| n.name == "a")
                .unwrap()
                .tick
        };
        assert_eq!(tick_a(&sel), Tick::Off);
        assert!(sel.iter().all(|id| {
            !plan.changes[id.0 as usize]
                .change
                .path()
                .is_within(&rel("a"))
        }));
        tree.toggle(&plan, &rel("a/b/1.txt"), &mut sel);
        assert_eq!(tick_a(&sel), Tick::Mixed);
        tree.toggle(&plan, &rel("a"), &mut sel); // mixed → all on
        assert_eq!(tick_a(&sel), Tick::On);
    }

    #[test]
    fn toggling_the_root_never_selects_skipped_changes() {
        let (plan, tree) = fixture();
        let mut sel = HashSet::new();
        tree.toggle(&plan, &RelPath::root(), &mut sel);
        assert_eq!(sel, plan.actionable_ids());
        tree.toggle(&plan, &RelPath::root(), &mut sel);
        assert!(sel.is_empty());
    }

    #[test]
    fn folders_with_deletes_lists_every_ancestor() {
        let (plan, tree) = fixture();
        assert_eq!(tree.folders_with_deletes(&plan), vec![rel("a")]);
    }

    #[test]
    fn root_children_and_toggle_with_200k_changes_are_fast() {
        let changes: Vec<Change> = (0..200_000)
            .map(|i| create(&format!("d{:03}/f{i:06}.txt", i % 500), 1))
            .collect();
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let tree = PlanTree::new(&plan);
        let mut sel = plan.actionable_ids();
        let t = std::time::Instant::now();
        let root = tree.children(&plan, &RelPath::root(), &sel);
        tree.toggle(&plan, &RelPath::root(), &mut sel);
        let elapsed = t.elapsed();
        assert_eq!(root.len(), 500);
        assert!(sel.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "took {elapsed:?}"
        );
    }
}
