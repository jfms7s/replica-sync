//! The plan as a folder tree for the Preview: children of one folder at a
//! time, per-folder counts and tick states. The plan stays in Rust; the UI
//! never holds more than the rows it shows.

use replica_sync_core::model::{Change, RelPath};
use replica_sync_core::plan::{ChangeId, Plan};
use serde::Serialize;
use std::cmp::Ordering;
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

/// One folder's children: the first `limit` of them, and how many there are in all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildrenPage {
    pub nodes: Vec<NodeView>,
    pub total: usize,
}

struct Row {
    idx: usize,
}

/// The plan's change indexes in tree order, so each folder's rows are one
/// contiguous range.
pub struct PlanTree {
    rows: Vec<Row>,
}

/// Paths in tree order: byte order with `/` sorting before every other byte,
/// so `a/x` comes before `a-c` and a folder's rows are never split.
fn tree_order(a: &RelPath, b: &RelPath) -> Ordering {
    let key = |c: u8| if c == b'/' { 0 } else { c };
    a.as_str().bytes().map(key).cmp(b.as_str().bytes().map(key))
}

fn path_of<'a>(plan: &'a Plan, r: &Row) -> &'a RelPath {
    plan.changes[r.idx].change.path()
}

impl PlanTree {
    pub fn new(plan: &Plan) -> PlanTree {
        let mut rows: Vec<Row> = (0..plan.changes.len()).map(|idx| Row { idx }).collect();
        rows.sort_by(|a, b| tree_order(path_of(plan, a), path_of(plan, b)));
        PlanTree { rows }
    }

    /// Rows at `folder` or inside it, as an index range (the whole tree for the root).
    fn range(&self, plan: &Plan, folder: &RelPath) -> (usize, usize) {
        if folder.is_root() {
            return (0, self.rows.len());
        }
        let lo = self
            .rows
            .partition_point(|r| tree_order(path_of(plan, r), folder) == Ordering::Less);
        let len = self.rows[lo..]
            .iter()
            .take_while(|r| path_of(plan, r).is_within(folder))
            .count();
        (lo, lo + len)
    }

    fn actionable(plan: &Plan, idx: usize) -> bool {
        !matches!(plan.changes[idx].change, Change::Skipped { .. })
    }

    /// At most `limit` children are built; `total` counts them all.
    pub fn children(
        &self,
        plan: &Plan,
        folder: &RelPath,
        sel: &HashSet<ChangeId>,
        limit: usize,
    ) -> ChildrenPage {
        let (lo, hi) = self.range(plan, folder);
        let skip = if folder.is_root() {
            0
        } else {
            folder.as_str().len() + 1
        };
        let mut out = Vec::new();
        let mut total = 0;
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
            let (clo, chi) = self.range(plan, &child);
            if out.len() < limit {
                out.push(self.node(plan, &child, name, clo, chi, sel));
            }
            total += 1;
            i = chi.max(i + 1);
        }
        ChildrenPage { nodes: out, total }
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
        let (lo, hi) = self.range(plan, path);
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
    use replica_sync_core::model::CaseMode;
    use replica_sync_core::model::MoveKind;
    use replica_sync_core::plan::build_plan;
    use replica_sync_core::reason::SkipReason;

    const ALL: usize = usize::MAX;

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
        let root = tree.children(&plan, &RelPath::root(), &sel, ALL).nodes;
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
        let a = tree.children(&plan, &rel("a"), &sel, ALL).nodes;
        assert_eq!(names(&a), vec!["b", "old.txt"]);
    }

    #[test]
    fn toggling_a_folder_then_a_file_gives_off_then_mixed() {
        let (plan, tree) = fixture();
        let mut sel = plan.actionable_ids();
        tree.toggle(&plan, &rel("a"), &mut sel);
        let tick_a = |sel: &HashSet<ChangeId>| {
            tree.children(&plan, &RelPath::root(), sel, ALL)
                .nodes
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
    fn folders_with_deletes_lists_each_level_of_deep_nesting_once() {
        let del = |p: &str| Change::Delete {
            path: rel(p),
            size: 1,
            mtime_ns: 0,
        };
        let plan = build_plan(
            vec![
                del("a/b/c/d/1.txt"),
                del("a/b/2.txt"),
                del("x/3.txt"),
                del("top.txt"),
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        assert_eq!(
            tree.folders_with_deletes(&plan),
            vec![rel("a"), rel("a/b"), rel("a/b/c"), rel("a/b/c/d"), rel("x")]
        );
    }

    #[test]
    fn a_folder_that_is_itself_a_change_shows_it_and_its_children() {
        let plan = build_plan(
            vec![
                Change::MkDir { path: rel("n") },
                create("n/x.txt", 4),
                create("n/sub/y.txt", 2),
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let root = tree.children(&plan, &RelPath::root(), &sel, ALL).nodes;
        assert_eq!(names(&root), vec!["n"]);
        let n = &root[0];
        assert!(n.is_folder);
        assert!(matches!(
            n.changes.as_slice(),
            [RowChange {
                change: Change::MkDir { .. },
                ..
            }]
        ));
        assert_eq!(
            (n.counts.folders, n.counts.create, n.bytes_to_copy),
            (1, 2, 6)
        );
        // The folder's own change is on its parent's row, not among its children.
        let inside = tree.children(&plan, &rel("n"), &sel, ALL).nodes;
        assert_eq!(names(&inside), vec!["sub", "x.txt"]);
    }

    #[test]
    fn an_update_and_a_case_only_rename_of_one_file_share_its_row() {
        let plan = build_plan(
            vec![
                Change::Move {
                    from: rel("Top.txt"),
                    to: rel("top.txt"),
                    kind: MoveKind::File {
                        size: 5,
                        mtime_ns: 0,
                    },
                },
                Change::Update {
                    path: rel("top.txt"),
                    size: 5,
                    mtime_ns: 0,
                    replica_newer: false,
                },
            ],
            CaseMode::Insensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        let mut sel = plan.actionable_ids();
        let root = tree.children(&plan, &RelPath::root(), &sel, ALL).nodes;
        assert_eq!(names(&root), vec!["top.txt"]);
        assert!(!root[0].is_folder);
        assert_eq!((root[0].counts.moves, root[0].counts.update), (1, 1));
        assert_eq!(root[0].changes.len(), 2);
        tree.toggle(&plan, &rel("top.txt"), &mut sel);
        assert!(sel.is_empty(), "one tick covers both changes");
    }

    #[test]
    fn a_huge_folder_gives_the_first_children_and_the_full_count() {
        let changes: Vec<Change> = (0..2500)
            .map(|i| create(&format!("big/f{i:05}.txt"), 1))
            .collect();
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let page = tree.children(&plan, &rel("big"), &sel, 2000);
        assert_eq!((page.nodes.len(), page.total), (2000, 2500));
        assert_eq!(page.nodes[0].name, "f00000.txt");
        let all = tree.children(&plan, &rel("big"), &sel, ALL);
        assert_eq!((all.nodes.len(), all.total), (2500, 2500));
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(v["total"], 2500);
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
        let root = tree.children(&plan, &RelPath::root(), &sel, ALL).nodes;
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
