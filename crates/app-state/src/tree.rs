//! The plan as a folder tree for the Preview: children of one folder at a
//! time, per-folder counts and tick states. The plan stays in Rust; the UI
//! never holds more than the rows it shows.

use replica_sync_core::model::{Change, RelPath};
use replica_sync_core::plan::{ChangeId, Plan};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

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

/// How a folder's children are ordered before the cap is applied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    /// Tree order (by name).
    #[default]
    Name,
    /// Largest `bytes_to_copy` first (a folder counts its whole subtree), ties by name.
    Size,
}

/// What a find returned: the first `limit` matches and how many there are in all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FindPage {
    pub nodes: Vec<NodeView>,
    pub total_matches: usize,
}

/// One thing find can hit: a change's own file or folder, or a folder that
/// only appears as the parent of changes (a new `Trips/` holding photos).
struct FindEntry {
    /// The lowercased name.
    name: Box<str>,
    /// The row the entry was found at.
    row: usize,
    /// 0 for the row's own path, else the length of the row's path that is this folder.
    folder_len: usize,
}

struct Row {
    idx: usize,
}

/// The plan's change indexes in tree order, so each folder's rows are one
/// contiguous range.
pub struct PlanTree {
    rows: Vec<Row>,
    /// Everything find can hit, in tree order. Built on the first find and
    /// dropped with the tree.
    index: OnceLock<Vec<FindEntry>>,
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
        PlanTree {
            rows,
            index: OnceLock::new(),
        }
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

    /// At most `limit` children are built; `total` counts them all. With
    /// [`SortOrder::Size`] the order is applied before the cap, so the cap keeps
    /// the largest children and "not shown" hides the smallest.
    pub fn children(
        &self,
        plan: &Plan,
        folder: &RelPath,
        sel: &HashSet<ChangeId>,
        limit: usize,
        order: SortOrder,
    ) -> ChildrenPage {
        let (lo, hi) = self.range(plan, folder);
        let skip = if folder.is_root() {
            0
        } else {
            folder.as_str().len() + 1
        };
        // Each child: its name and its row range, in tree (name) order.
        let mut kids: Vec<(String, usize, usize)> = Vec::new();
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
            let (clo, chi) = self.range(plan, &folder.join(&name));
            kids.push((name, clo, chi));
            i = chi.max(i + 1);
        }
        let total = kids.len();
        if order == SortOrder::Size {
            let mut sized: Vec<(u64, (String, usize, usize))> = kids
                .into_iter()
                .map(|k| (self.bytes_in(plan, k.1, k.2), k))
                .collect();
            sized.sort_by(|(a, ka), (b, kb)| b.cmp(a).then_with(|| ka.0.cmp(&kb.0)));
            kids = sized.into_iter().map(|(_, k)| k).collect();
        }
        let nodes = kids
            .into_iter()
            .take(limit)
            .map(|(name, clo, chi)| {
                let child = folder.join(&name);
                self.node(plan, &child, name, clo, chi, sel)
            })
            .collect();
        ChildrenPage { nodes, total }
    }

    /// What `node` reports as `bytes_to_copy` for the rows `lo..hi`.
    fn bytes_in(&self, plan: &Plan, lo: usize, hi: usize) -> u64 {
        self.rows[lo..hi]
            .iter()
            .map(|r| plan.changes[r.idx].change.bytes_to_copy())
            .sum()
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

    /// Every change's own name, plus every folder above a change once (rows
    /// are in tree order, so a folder's rows are contiguous and it is met first
    /// at its first row).
    fn build_index(&self, plan: &Plan) -> Vec<FindEntry> {
        let entry = |name: &str, row: usize, folder_len: usize| FindEntry {
            name: name.to_lowercase().into_boxed_str(),
            row,
            folder_len,
        };
        let mut out = Vec::new();
        let mut prev: Option<&RelPath> = None;
        let mut prev_slashes: Vec<usize> = Vec::new();
        for (row, r) in self.rows.iter().enumerate() {
            let path = path_of(plan, r);
            let s = path.as_str();
            let slashes: Vec<usize> = s.match_indices('/').map(|(at, _)| at).collect();
            // The folders above this row that the previous row was not already inside.
            let prev_s = prev.map_or("", RelPath::as_str);
            let shared = slashes
                .iter()
                .zip(&prev_slashes)
                .take_while(|(a, b)| a == b && s[..**a] == prev_s[..**a])
                .count();
            for &at in &slashes[shared..] {
                // A folder that is itself the previous row's change is already an entry.
                if prev_s != &s[..at] {
                    out.push(entry(
                        s[..at].rsplit('/').next().unwrap_or_default(),
                        row,
                        at,
                    ));
                }
            }
            // A file with two changes (a rename and an update) is one entry.
            if prev != Some(path) {
                out.push(entry(path.name(), row, 0));
            }
            prev = Some(path);
            prev_slashes = slashes;
        }
        out
    }

    /// A lookup, never a filter: the changes whose own file or folder name
    /// contains `query` (case-insensitive, the name only, not the folder path),
    /// in tree order, at most `limit` of them. Each result is the same node the
    /// tree draws for that path, so its tick shows what `toggle` on that path
    /// flips. Nothing here reads or changes the selection; "Approve all" and the
    /// totals still cover the whole plan. A blank query finds nothing.
    pub fn find(
        &self,
        plan: &Plan,
        query: &str,
        sel: &HashSet<ChangeId>,
        limit: usize,
    ) -> FindPage {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return FindPage {
                nodes: Vec::new(),
                total_matches: 0,
            };
        }
        let index = self.index.get_or_init(|| self.build_index(plan));
        let mut nodes = Vec::new();
        let mut total_matches = 0;
        for e in index.iter().filter(|e| e.name.contains(&needle)) {
            total_matches += 1;
            if nodes.len() < limit {
                let row_path = path_of(plan, &self.rows[e.row]);
                let path = if e.folder_len == 0 {
                    row_path.clone()
                } else {
                    match RelPath::new(&row_path.as_str()[..e.folder_len]) {
                        Ok(p) => p,
                        Err(_) => continue,
                    }
                };
                let (lo, hi) = self.range(plan, &path);
                nodes.push(self.node(plan, &path, path.name().to_owned(), lo, hi, sel));
            }
        }
        FindPage {
            nodes,
            total_matches,
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
                    replica_size: 0,
                    replica_mtime_ns: 0,
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
        let root = tree
            .children(&plan, &RelPath::root(), &sel, ALL, SortOrder::Name)
            .nodes;
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
        let a = tree
            .children(&plan, &rel("a"), &sel, ALL, SortOrder::Name)
            .nodes;
        assert_eq!(names(&a), vec!["b", "old.txt"]);
    }

    #[test]
    fn toggling_a_folder_then_a_file_gives_off_then_mixed() {
        let (plan, tree) = fixture();
        let mut sel = plan.actionable_ids();
        tree.toggle(&plan, &rel("a"), &mut sel);
        let tick_a = |sel: &HashSet<ChangeId>| {
            tree.children(&plan, &RelPath::root(), sel, ALL, SortOrder::Name)
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
        let root = tree
            .children(&plan, &RelPath::root(), &sel, ALL, SortOrder::Name)
            .nodes;
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
        let inside = tree
            .children(&plan, &rel("n"), &sel, ALL, SortOrder::Name)
            .nodes;
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
                    replica_size: 0,
                    replica_mtime_ns: 0,
                    replica_newer: false,
                },
            ],
            CaseMode::Insensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        let mut sel = plan.actionable_ids();
        let root = tree
            .children(&plan, &RelPath::root(), &sel, ALL, SortOrder::Name)
            .nodes;
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
        let page = tree.children(&plan, &rel("big"), &sel, 2000, SortOrder::Name);
        assert_eq!((page.nodes.len(), page.total), (2000, 2500));
        assert_eq!(page.nodes[0].name, "f00000.txt");
        let all = tree.children(&plan, &rel("big"), &sel, ALL, SortOrder::Name);
        assert_eq!((all.nodes.len(), all.total), (2500, 2500));
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(v["total"], 2500);
    }

    #[test]
    fn size_order_lists_children_by_descending_subtree_bytes_ties_by_name() {
        let plan = build_plan(
            vec![
                create("d/small.txt", 5),
                create("d/big.bin", 100),
                create("d/b-same.txt", 7),
                create("d/a-same.txt", 7),
                create("d/zero.txt", 0),
                // a folder whose subtree (60 + 70) beats a single 100-byte file
                create("d/sub/x.bin", 60),
                create("d/sub/deep/y.bin", 70),
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let by_size = tree.children(&plan, &rel("d"), &sel, ALL, SortOrder::Size);
        assert_eq!(
            names(&by_size.nodes),
            vec![
                "sub",
                "big.bin",
                "a-same.txt",
                "b-same.txt",
                "small.txt",
                "zero.txt"
            ]
        );
        let bytes: Vec<u64> = by_size.nodes.iter().map(|n| n.bytes_to_copy).collect();
        assert_eq!(bytes, vec![130, 100, 7, 7, 5, 0]);
        let by_name = tree.children(&plan, &rel("d"), &sel, ALL, SortOrder::Name);
        assert_eq!(
            names(&by_name.nodes),
            vec![
                "a-same.txt",
                "b-same.txt",
                "big.bin",
                "small.txt",
                "sub",
                "zero.txt"
            ]
        );
        assert_eq!(by_size.total, by_name.total);
    }

    #[test]
    fn size_order_applies_before_the_cap_and_keeps_the_largest() {
        let mut changes: Vec<Change> = (0..2500)
            .map(|i| create(&format!("big/f{i:05}.txt"), 1))
            .collect();
        // the five biggest sort last by name
        changes.extend((0..5).map(|i| create(&format!("big/zz{i}.bin"), 1000 + i)));
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let by_name = tree.children(&plan, &rel("big"), &sel, 2000, SortOrder::Name);
        assert!(by_name.nodes.iter().all(|n| !n.name.starts_with("zz")));
        let by_size = tree.children(&plan, &rel("big"), &sel, 2000, SortOrder::Size);
        assert_eq!((by_size.nodes.len(), by_size.total), (2000, 2505));
        assert_eq!(
            names(&by_size.nodes)[..5],
            ["zz4.bin", "zz3.bin", "zz2.bin", "zz1.bin", "zz0.bin"]
        );
        // what is hidden is the smallest: every shown child is at least as big as every hidden one
        assert!(by_size.nodes.iter().all(|n| n.bytes_to_copy >= 1));
        assert_eq!(by_size.nodes[1999].name, "f01994.txt");
    }

    #[test]
    fn size_order_changes_only_the_order_never_ticks_or_totals() {
        let (plan, tree) = fixture();
        let mut sel = plan.actionable_ids();
        tree.toggle(&plan, &rel("a/b/1.txt"), &mut sel);
        for folder in [RelPath::root(), rel("a"), rel("a/b")] {
            let mut n = tree.children(&plan, &folder, &sel, ALL, SortOrder::Name);
            let mut s = tree.children(&plan, &folder, &sel, ALL, SortOrder::Size);
            n.nodes.sort_by(|a, b| a.name.cmp(&b.name));
            s.nodes.sort_by(|a, b| a.name.cmp(&b.name));
            assert_eq!(n, s);
        }
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
        let root = tree
            .children(&plan, &RelPath::root(), &sel, ALL, SortOrder::Name)
            .nodes;
        tree.toggle(&plan, &RelPath::root(), &mut sel);
        let elapsed = t.elapsed();
        assert_eq!(root.len(), 500);
        assert!(sel.is_empty());
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "took {elapsed:?}"
        );
    }

    #[test]
    fn size_order_over_200k_changes_costs_the_same_order_as_name_order() {
        // One folder with 200k files: Size must look at every child, Name only builds `limit`.
        let changes: Vec<Change> = (0..200_000u64)
            .map(|i| create(&format!("big/f{i:06}.txt"), (i * 7919) % 10_007))
            .collect();
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let best = |order: SortOrder| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let page = tree.children(&plan, &rel("big"), &sel, 2000, order);
                    let elapsed = t.elapsed();
                    assert_eq!((page.nodes.len(), page.total), (2000, 200_000));
                    elapsed
                })
                .min()
                .unwrap()
        };
        let (name, size) = (best(SortOrder::Name), best(SortOrder::Size));
        // A ratio, never an absolute time: CI machines differ.
        assert!(size < name * 20, "name {name:?}, size {size:?}");
        let page = tree.children(&plan, &rel("big"), &sel, 2000, SortOrder::Size);
        assert!(
            page.nodes
                .windows(2)
                .all(|w| w[0].bytes_to_copy >= w[1].bytes_to_copy)
        );
    }

    // ---- find ----

    fn find_names(page: &FindPage) -> Vec<&str> {
        page.nodes.iter().map(|n| n.path.as_str()).collect()
    }

    fn find_fixture() -> (Plan, PlanTree) {
        let plan = build_plan(
            vec![
                create("Photos/IMG_0001.jpg", 1),
                create("Photos/IMG_0002.JPG", 2),
                create("Photos/notes.txt", 4),
                create("photos-old/a.txt", 8),
                create("docs/img.md", 16),
                Change::MkDir {
                    path: rel("Photos/img-sets"),
                },
                create("Photos/img-sets/x.txt", 32),
                Change::Skipped {
                    path: rel("Photos/img-link"),
                    reason: SkipReason::Link,
                },
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        (plan, tree)
    }

    #[test]
    fn find_matches_the_name_case_insensitively_not_the_folder_path() {
        let (plan, tree) = find_fixture();
        let sel = plan.actionable_ids();
        let page = tree.find(&plan, "img", &sel, 200);
        assert_eq!(
            find_names(&page),
            vec![
                "Photos/IMG_0001.jpg",
                "Photos/IMG_0002.JPG",
                "Photos/img-link",
                "Photos/img-sets",
                "docs/img.md",
            ]
        );
        assert_eq!(page.total_matches, 5);
        // "photos" matches the two folders by their own names; it never matches a
        // file just because of the folder it sits in.
        let photos = tree.find(&plan, "PHOTOS", &sel, 200);
        assert_eq!(find_names(&photos), vec!["Photos", "photos-old"]);
        assert!(photos.nodes.iter().all(|n| n.is_folder));
        assert_eq!(
            find_names(&tree.find(&plan, "  Notes ", &sel, 200)),
            vec!["Photos/notes.txt"]
        );
        assert_eq!(
            find_names(&tree.find(&plan, "0002", &sel, 200)),
            vec!["Photos/IMG_0002.JPG"]
        );
    }

    #[test]
    fn a_blank_query_finds_nothing() {
        let (plan, tree) = find_fixture();
        let sel = plan.actionable_ids();
        for q in ["", " ", "\t \n"] {
            let page = tree.find(&plan, q, &sel, 200);
            assert_eq!((page.nodes.len(), page.total_matches), (0, 0));
        }
    }

    #[test]
    fn find_caps_the_results_and_counts_every_match() {
        let changes: Vec<Change> = (0..500)
            .map(|i| create(&format!("d{}/f{i:03}.txt", i % 7), 1))
            .collect();
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let page = tree.find(&plan, "f", &sel, 200);
        assert_eq!((page.nodes.len(), page.total_matches), (200, 500));
        let none = tree.find(&plan, "f", &sel, 0);
        assert_eq!((none.nodes.len(), none.total_matches), (0, 500));
    }

    #[test]
    fn find_results_carry_the_same_tick_the_tree_shows_and_never_change_it() {
        let (plan, tree) = find_fixture();
        let mut sel = plan.actionable_ids();
        tree.toggle(&plan, &rel("Photos/IMG_0002.JPG"), &mut sel);
        let before = sel.clone();
        let page = tree.find(&plan, "img_000", &sel, 200);
        let ticks: Vec<(&str, Tick)> = page
            .nodes
            .iter()
            .map(|n| (n.name.as_str(), n.tick))
            .collect();
        assert_eq!(
            ticks,
            vec![("IMG_0001.jpg", Tick::On), ("IMG_0002.JPG", Tick::Off)]
        );
        assert_eq!(sel, before, "find is a lookup: the selection is untouched");
        // A skipped change is found, but cannot be ticked.
        let link = tree.find(&plan, "img-link", &sel, 200);
        assert_eq!(link.nodes[0].tick, Tick::None);
        // A folder that is itself a change reports (and toggles) its whole subtree.
        let dir = tree.find(&plan, "img-sets", &sel, 200).nodes.remove(0);
        assert!(dir.is_folder);
        assert_eq!((dir.counts.folders, dir.counts.create), (1, 1));
    }

    #[test]
    fn ticking_a_found_file_flips_exactly_that_files_changes() {
        let (plan, tree) = find_fixture();
        let mut sel = plan.actionable_ids();
        let total = sel.len();
        let hit = tree.find(&plan, "img_0001", &sel, 200).nodes.remove(0);
        tree.toggle(&plan, &hit.path, &mut sel);
        assert_eq!(sel.len(), total - 1);
        let again = tree.find(&plan, "img_0001", &sel, 200).nodes.remove(0);
        assert_eq!(again.tick, Tick::Off);
        tree.toggle(&plan, &again.path, &mut sel);
        assert_eq!(sel, plan.actionable_ids());
    }

    #[test]
    fn a_file_with_a_rename_and_an_update_is_one_result() {
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
                    replica_size: 0,
                    replica_mtime_ns: 0,
                    replica_newer: false,
                },
            ],
            CaseMode::Insensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        let page = tree.find(&plan, "TOP", &plan.actionable_ids(), 200);
        assert_eq!((page.nodes.len(), page.total_matches), (1, 1));
        assert_eq!(page.nodes[0].changes.len(), 2);
    }

    #[test]
    fn find_over_200k_changes_costs_the_same_order_as_a_children_walk() {
        let build = || {
            let changes: Vec<Change> = (0..200_000)
                .map(|i| create(&format!("d{:03}/f{i:06}.txt", i % 500), 1))
                .collect();
            let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
            let tree = PlanTree::new(&plan);
            (plan, tree)
        };
        let (plan, tree) = build();
        let sel = plan.actionable_ids();
        // Reference: one full pass over the same rows (the root's children).
        let walk = (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let root = tree.children(&plan, &RelPath::root(), &sel, ALL, SortOrder::Name);
                let e = t.elapsed();
                assert_eq!(root.nodes.len(), 500);
                e
            })
            .min()
            .unwrap();
        // The first find builds the name index, so time that one on a fresh tree each round.
        let first = (0..3)
            .map(|_| {
                let fresh = PlanTree::new(&plan);
                let t = std::time::Instant::now();
                let page = fresh.find(&plan, "f1999", &sel, 200);
                let e = t.elapsed();
                assert_eq!(page.total_matches, 100);
                e
            })
            .min()
            .unwrap();
        // Later finds reuse the index.
        let again = (0..3)
            .map(|_| {
                let t = std::time::Instant::now();
                let page = tree.find(&plan, "f19", &sel, 200);
                let e = t.elapsed();
                assert_eq!((page.nodes.len(), page.total_matches), (200, 10_000));
                e
            })
            .min()
            .unwrap();
        let _ = tree.find(&plan, "f", &sel, 1);
        // Ratios to the walk, never an absolute time: CI machines differ.
        assert!(first < walk * 20, "walk {walk:?}, first find {first:?}");
        assert!(again < walk * 20, "walk {walk:?}, later find {again:?}");
    }

    #[test]
    fn find_reaches_folders_that_only_hold_changes() {
        // What a real scan produces: no MkDir for a folder with files in it.
        let plan = build_plan(
            vec![
                create("Trips/Lisbon/a.jpg", 1),
                create("Trips/Lisbon/b.jpg", 2),
                create("Trips/c.jpg", 4),
                create("Trips2/d.jpg", 8),
                create("zeta/trips-notes.txt", 16),
                create("other.txt", 32),
            ],
            CaseMode::Sensitive,
        )
        .unwrap();
        let tree = PlanTree::new(&plan);
        let mut sel = plan.actionable_ids();
        let page = tree.find(&plan, "trips", &sel, 200);
        assert_eq!(
            find_names(&page),
            vec!["Trips", "Trips2", "zeta/trips-notes.txt"]
        );
        assert_eq!(page.total_matches, 3);
        let trips = &page.nodes[0];
        assert!(trips.is_folder);
        assert_eq!((trips.counts.create, trips.bytes_to_copy), (3, 7));
        assert_eq!(trips.tick, Tick::On);
        // A nested folder is found too, once, with its own subtree.
        let lisbon = tree.find(&plan, "lisb", &sel, 200).nodes.remove(0);
        assert_eq!(
            (lisbon.path.as_str(), lisbon.counts.create),
            ("Trips/Lisbon", 2)
        );
        // Ticking the folder hit flips exactly its subtree (3 of 6 changes) and nothing else.
        let total = plan.actionable_ids();
        tree.toggle(&plan, &trips.path, &mut sel);
        assert_eq!(sel.len(), 3);
        for id in &total {
            let inside = plan.changes[id.0 as usize]
                .change
                .path()
                .is_within(&rel("Trips"));
            assert_eq!(sel.contains(id), !inside);
        }
        assert_eq!(
            tree.find(&plan, "trips", &sel, 200).nodes[0].tick,
            Tick::Off
        );
        assert_eq!(tree.find(&plan, "lisb", &sel, 200).nodes[0].tick, Tick::Off);
        // The tree shows the same tick for the same folder.
        let in_tree = tree.children(&plan, &RelPath::root(), &sel, ALL, SortOrder::Name);
        assert_eq!(in_tree.nodes[0].tick, Tick::Off);
        // Find itself never touched the selection, so Approve all is still the whole plan.
        assert_eq!(sel.len(), 3);
        assert_eq!(plan.actionable_ids().len(), 6);
    }

    #[test]
    fn a_folder_is_one_result_whatever_number_of_changes_it_holds() {
        let changes: Vec<Change> = (0..50)
            .map(|i| create(&format!("hits/sub{}/f{i}.txt", i % 5), 1))
            .collect();
        let plan = build_plan(changes, CaseMode::Sensitive).unwrap();
        let tree = PlanTree::new(&plan);
        let sel = plan.actionable_ids();
        let page = tree.find(&plan, "sub", &sel, 200);
        assert_eq!(page.total_matches, 5);
        assert_eq!(tree.find(&plan, "hits", &sel, 200).total_matches, 1);
        // The cap and the total still apply to folder hits.
        let capped = tree.find(&plan, "sub", &sel, 2);
        assert_eq!((capped.nodes.len(), capped.total_matches), (2, 5));
    }
}
