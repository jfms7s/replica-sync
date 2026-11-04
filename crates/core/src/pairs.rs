//! Saved source → replica pairs, found by drive ID rather than drive letter.

use crate::model::RelPath;
use crate::rules::{RuleError, SkipRules};
use crate::safety::canonical;
use crate::volume::{self, Volumes};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_TRASH_DAYS: u32 = 30;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    pub volume_id: String,
    /// `/`-separated path from the drive's mount root; empty for the root itself.
    /// Validated like any [`RelPath`] on load (same JSON string as before).
    pub rel_path: RelPath,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSync {
    /// RFC 3339.
    pub at: String,
    pub applied: usize,
    pub failed: usize,
    /// The run stopped before doing every approved change.
    #[serde(default)]
    pub stopped: bool,
}

/// Most backup disks one pair may rotate between ("one at home, one at a
/// relative's"). The store keeps a list, so raising this needs no migration.
pub const MAX_REPLICAS: usize = 2;

/// One backup disk of a pair, with its own sync history: each disk is a mirror
/// that is only as current as the last run that reached it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaSide {
    pub side: Side,
    pub last_sync: Option<LastSync>,
    /// Files seen in the last source scan for this disk, for an approximate
    /// progress bar.
    pub last_scan_files: Option<u64>,
}

impl ReplicaSide {
    fn new(side: Side) -> ReplicaSide {
        ReplicaSide {
            side,
            last_sync: None,
            last_scan_files: None,
        }
    }

    /// Never synced to this disk: the mass-change guard then suspects the
    /// replica folder rather than the source. Another disk having synced does
    /// not count. A record whose time can't be read proves nothing, so it
    /// counts as never.
    pub fn is_first_sync(&self) -> bool {
        self.last_sync
            .as_ref()
            .is_none_or(|l| chrono::DateTime::parse_from_rfc3339(&l.at).is_err())
    }
}

/// `replicas` holds 1 to [`MAX_REPLICAS`] disks, always: creation, the editing
/// functions and loading all enforce it, so it is not a public field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PairWire")]
pub struct Pair {
    pub id: String,
    pub name: String,
    pub source: Side,
    replicas: Vec<ReplicaSide>,
    pub user_rules: Vec<String>,
    pub trash_days: u32,
}

/// A pair as `pairs.json` may hold it. Format 1 (every release up to 0.7) has
/// one `replica` and the pair's own `last_sync` / `last_scan_files`; format 2
/// has `replicas`. Both are read, forever; only format 2 is written.
#[derive(Deserialize)]
struct PairWire {
    id: String,
    name: String,
    source: Side,
    #[serde(default)]
    replicas: Option<Vec<ReplicaSide>>,
    #[serde(default)]
    replica: Option<Side>,
    user_rules: Vec<String>,
    trash_days: u32,
    #[serde(default)]
    last_sync: Option<LastSync>,
    #[serde(default)]
    last_scan_files: Option<u64>,
}

impl TryFrom<PairWire> for Pair {
    type Error = String;

    fn try_from(w: PairWire) -> Result<Pair, String> {
        let replicas = match (w.replicas, w.replica) {
            (Some(_), Some(_)) => {
                return Err(format!("pair {}: has both `replica` and `replicas`", w.id));
            }
            (Some(list), None) => {
                if w.last_sync.is_some() || w.last_scan_files.is_some() {
                    return Err(format!(
                        "pair {}: `replicas` with a pair-level last_sync or last_scan_files",
                        w.id
                    ));
                }
                if !(1..=MAX_REPLICAS).contains(&list.len()) {
                    return Err(format!(
                        "pair {}: {} replicas, expected 1 to {MAX_REPLICAS}",
                        w.id,
                        list.len()
                    ));
                }
                list
            }
            // The single disk takes over the pair's own history.
            (None, Some(side)) => vec![ReplicaSide {
                side,
                last_sync: w.last_sync,
                last_scan_files: w.last_scan_files,
            }],
            (None, None) => return Err(format!("pair {}: has no replica", w.id)),
        };
        Ok(Pair {
            id: w.id,
            name: w.name,
            source: w.source,
            replicas,
            user_rules: w.user_rules,
            trash_days: w.trash_days,
        })
    }
}

impl Pair {
    pub fn rules(&self) -> Result<SkipRules, RuleError> {
        SkipRules::new(&self.user_rules)
    }

    /// The backup disks, 1 to [`MAX_REPLICAS`], in the order they were added.
    pub fn replicas(&self) -> &[ReplicaSide] {
        &self.replicas
    }

    pub fn replica(&self, index: usize) -> Option<&ReplicaSide> {
        self.replicas.get(index)
    }

    pub fn replica_mut(&mut self, index: usize) -> Option<&mut ReplicaSide> {
        self.replicas.get_mut(index)
    }
}

pub use crate::model::SideKind;

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("the source and the replica are the same folder")]
    SameFolder,
    #[error("one folder is inside the other")]
    Nested,
    #[error("the source and the replica are on the same drive; confirm to continue")]
    SameVolume,
    #[error("a pair has at most {MAX_REPLICAS} backup disks")]
    TooManyReplicas,
    #[error("a pair needs at least one backup disk")]
    LastReplica,
    #[error("the pair has no backup disk number {0}")]
    NoSuchReplica(usize),
    #[error(transparent)]
    Rule(#[from] RuleError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub struct NewPair<'a> {
    pub name: &'a str,
    pub source: &'a Path,
    pub replica: &'a Path,
    pub user_rules: Vec<String>,
    pub trash_days: u32,
    pub allow_same_volume: bool,
}

/// The stored side for `path`, plus its canonical path for comparing with the
/// other side. Both canonical paths come from [`canonical`] (always verbatim
/// on Windows), so a long path still strips and compares correctly.
fn side_for(path: &Path, volumes: &dyn Volumes) -> Result<(Side, PathBuf), PairError> {
    let canonical_path = canonical(path)?;
    let vol = volumes.volume_of(&canonical_path)?;
    let mount_root = canonical(&vol.mount_root)?;
    let rest = canonical_path
        .strip_prefix(&mount_root)
        .map_err(|_| io::Error::other("folder is not under its drive's mount point"))?;
    let names = rest
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: folder name is not valid Unicode", path.display()),
            )
        })?;
    let rel_path = RelPath::try_from(names.join("/")).map_err(io::Error::other)?;
    Ok((
        Side {
            volume_id: vol.id,
            rel_path,
            label: vol.label,
        },
        canonical_path,
    ))
}

fn check_folders(a: &Path, b: &Path) -> Result<(), PairError> {
    match crate::safety::nesting(a, b) {
        Some(crate::safety::Nesting::Same) => Err(PairError::SameFolder),
        Some(crate::safety::Nesting::Nested) => Err(PairError::Nested),
        None => Ok(()),
    }
}

/// The clock in nanoseconds as hex, bumped past the last id this process made,
/// so a coarse clock (100 ns steps on Windows) never repeats one.
fn new_id() -> String {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let prev = LAST
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |last| {
            Some(now.max(last + 1))
        })
        .expect("the update always returns Some");
    format!("{:x}", now.max(prev + 1))
}

pub fn create_pair(req: NewPair, volumes: &dyn Volumes) -> Result<Pair, PairError> {
    SkipRules::new(&req.user_rules)?;
    let (source, s) = side_for(req.source, volumes)?;
    let (replica, r) = side_for(req.replica, volumes)?;
    check_folders(&s, &r)?;
    if source.volume_id == replica.volume_id && !req.allow_same_volume {
        return Err(PairError::SameVolume);
    }
    Ok(Pair {
        id: new_id(),
        name: req.name.trim().to_owned(),
        source,
        replicas: vec![ReplicaSide::new(replica)],
        user_rules: req.user_rules,
        trash_days: req.trash_days,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolved {
    pub source_root: PathBuf,
    pub replica_root: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("{label} is not connected")]
    NotConnected { side: SideKind, label: String },
    #[error("the folder {} no longer exists", .0.display())]
    FolderMissing(PathBuf),
    #[error("the drive at {} is not the one saved for this pair", .0.display())]
    WrongVolume(PathBuf),
    #[error("the pair has no backup disk number {0}")]
    NoSuchReplica(usize),
    #[error(transparent)]
    Io(#[from] io::Error),
}

fn resolve_side(
    side: &Side,
    kind: SideKind,
    volumes: &dyn Volumes,
) -> Result<PathBuf, ResolveError> {
    let vol = volumes
        .find(&side.volume_id)?
        .ok_or_else(|| ResolveError::NotConnected {
            side: kind,
            label: side.label.clone(),
        })?;
    let root = side.rel_path.to_path(&vol.mount_root);
    if !root.is_dir() {
        return Err(ResolveError::FolderMissing(root));
    }
    Ok(root)
}

fn replica_side(pair: &Pair, replica: usize) -> Result<&Side, ResolveError> {
    pair.replica(replica)
        .map(|r| &r.side)
        .ok_or(ResolveError::NoSuchReplica(replica))
}

/// The source and backup disk number `replica` of `pair`.
pub fn resolve(
    pair: &Pair,
    replica: usize,
    volumes: &dyn Volumes,
) -> Result<Resolved, ResolveError> {
    let replica = replica_side(pair, replica)?;
    Ok(Resolved {
        source_root: resolve_side(&pair.source, SideKind::Source, volumes)?,
        replica_root: resolve_side(replica, SideKind::Replica, volumes)?,
    })
}

fn verify_side(
    side: &Side,
    kind: SideKind,
    root: &Path,
    volumes: &dyn Volumes,
) -> Result<(), ResolveError> {
    match volumes.volume_of(root) {
        Ok(v) if volume::id_matches(&side.volume_id, &v.id) => Ok(()),
        Ok(_) => Err(ResolveError::WrongVolume(root.to_path_buf())),
        // The folder is gone: missing if its drive is still there.
        Err(e) if e.kind() == io::ErrorKind::NotFound => match volumes.find(&side.volume_id)? {
            Some(_) => Err(ResolveError::FolderMissing(root.to_path_buf())),
            None => Err(ResolveError::NotConnected {
                side: kind,
                label: side.label.clone(),
            }),
        },
        Err(e) => Err(e.into()),
    }
}

/// Re-checked right before Apply: a different disk on the same letter is refused.
pub fn verify_replica(
    pair: &Pair,
    replica: usize,
    resolved: &Resolved,
    volumes: &dyn Volumes,
) -> Result<(), ResolveError> {
    verify_side(
        replica_side(pair, replica)?,
        SideKind::Replica,
        &resolved.replica_root,
        volumes,
    )
}

/// Re-checked right before Apply. On Linux an unmounted source is still an empty
/// folder on the root filesystem; its drive id then differs, so this refuses it.
pub fn verify_source(
    pair: &Pair,
    resolved: &Resolved,
    volumes: &dyn Volumes,
) -> Result<(), ResolveError> {
    verify_side(
        &pair.source,
        SideKind::Source,
        &resolved.source_root,
        volumes,
    )
}

/// Backup disk number `replica`'s folder alone, for screens (Trash) that don't
/// need the source.
pub fn resolve_replica(
    pair: &Pair,
    replica: usize,
    volumes: &dyn Volumes,
) -> Result<PathBuf, ResolveError> {
    resolve_side(replica_side(pair, replica)?, SideKind::Replica, volumes)
}

/// Refuses `new_path` when it is the same folder as `other`, or nested with it.
/// While `other` is away it can't be checked; resolve checks again.
fn check_against(
    new_path: &Path,
    other: &Side,
    kind: SideKind,
    volumes: &dyn Volumes,
) -> Result<(), PairError> {
    match resolve_side(other, kind, volumes) {
        Ok(other_path) => check_folders(new_path, &canonical(&other_path)?),
        Err(ResolveError::NotConnected { .. } | ResolveError::FolderMissing(_)) => Ok(()),
        Err(ResolveError::Io(e)) => Err(PairError::Io(e)),
        Err(e @ (ResolveError::WrongVolume(_) | ResolveError::NoSuchReplica(_))) => {
            unreachable!("resolve_side neither checks the drive nor picks a disk: {e}")
        }
    }
}

/// Checks a new backup folder against the source and the other backup disks.
/// `skip` is the disk being replaced, if any.
fn check_replica(
    pair: &Pair,
    skip: Option<usize>,
    new_side: &Side,
    new_path: &Path,
    allow_same_volume: bool,
    volumes: &dyn Volumes,
) -> Result<(), PairError> {
    check_against(new_path, &pair.source, SideKind::Source, volumes)?;
    let others = (0..pair.replicas.len()).filter(|i| Some(*i) != skip);
    for other in others.map(|i| &pair.replicas[i]) {
        check_against(new_path, &other.side, SideKind::Replica, volumes)?;
    }
    if pair.source.volume_id == new_side.volume_id && !allow_same_volume {
        return Err(PairError::SameVolume);
    }
    Ok(())
}

/// Points the source at `folder`. A folder on a backup disk's drive is refused
/// unless `allow_same_volume`. Every disk's history is dropped: with another
/// source, the next sync to each is a first sync.
pub fn relink_source(
    pair: &mut Pair,
    folder: &Path,
    allow_same_volume: bool,
    volumes: &dyn Volumes,
) -> Result<(), PairError> {
    let (new_side, new_path) = side_for(folder, volumes)?;
    for r in &pair.replicas {
        check_against(&new_path, &r.side, SideKind::Replica, volumes)?;
    }
    let same_volume = pair
        .replicas
        .iter()
        .any(|r| r.side.volume_id == new_side.volume_id);
    if same_volume && !allow_same_volume {
        return Err(PairError::SameVolume);
    }
    pair.source = new_side;
    for r in &mut pair.replicas {
        *r = ReplicaSide::new(r.side.clone());
    }
    Ok(())
}

/// Points backup disk number `replica` at `folder`, like [`create_pair`]: a
/// folder on the source's drive is refused unless `allow_same_volume`. That
/// disk's history is dropped (its next sync is a first sync); the other disk's
/// is kept.
pub fn relink_replica(
    pair: &mut Pair,
    replica: usize,
    folder: &Path,
    allow_same_volume: bool,
    volumes: &dyn Volumes,
) -> Result<(), PairError> {
    if pair.replica(replica).is_none() {
        return Err(PairError::NoSuchReplica(replica));
    }
    let (new_side, new_path) = side_for(folder, volumes)?;
    check_replica(
        pair,
        Some(replica),
        &new_side,
        &new_path,
        allow_same_volume,
        volumes,
    )?;
    pair.replicas[replica] = ReplicaSide::new(new_side);
    Ok(())
}

/// Adds a second backup disk, which starts with no history.
pub fn add_replica(
    pair: &mut Pair,
    folder: &Path,
    allow_same_volume: bool,
    volumes: &dyn Volumes,
) -> Result<(), PairError> {
    if pair.replicas.len() >= MAX_REPLICAS {
        return Err(PairError::TooManyReplicas);
    }
    let (new_side, new_path) = side_for(folder, volumes)?;
    check_replica(pair, None, &new_side, &new_path, allow_same_volume, volumes)?;
    pair.replicas.push(ReplicaSide::new(new_side));
    Ok(())
}

/// Drops backup disk number `replica` and its history. The last disk can't be
/// removed.
pub fn remove_replica(pair: &mut Pair, replica: usize) -> Result<(), PairError> {
    if pair.replica(replica).is_none() {
        return Err(PairError::NoSuchReplica(replica));
    }
    if pair.replicas.len() == 1 {
        return Err(PairError::LastReplica);
    }
    pair.replicas.remove(replica);
    Ok(())
}

/// The `pairs.json` format this build reads and writes.
const STORE_VERSION: u32 = 2;

#[derive(Serialize)]
struct StoreOut<'a> {
    version: u32,
    pairs: &'a [Pair],
}

#[derive(Deserialize)]
struct StoreIn {
    /// A store without it is read as format 1. Format 1 (one `replica` per
    /// pair) and 2 (`replicas`) are both read; only 2 is written.
    #[serde(default = "first_store_version")]
    version: u32,
    pairs: Vec<Pair>,
}

fn first_store_version() -> u32 {
    1
}

#[derive(Debug)]
pub struct PairStore {
    path: PathBuf,
    pub pairs: Vec<Pair>,
}

impl PairStore {
    pub fn load(path: &Path) -> io::Result<PairStore> {
        let pairs = match fs::read_to_string(path) {
            Ok(text) => {
                let store = serde_json::from_str::<StoreIn>(&text).map_err(io::Error::other)?;
                if store.version > STORE_VERSION {
                    return Err(io::Error::other(format!(
                        "{} was written by a newer replica-sync (format {})",
                        path.display(),
                        store.version
                    )));
                }
                store.pairs
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(PairStore {
            path: path.to_path_buf(),
            pairs,
        })
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            crate::private::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&StoreOut {
            version: STORE_VERSION,
            pairs: &self.pairs,
        })
        .map_err(io::Error::other)?;
        // A leftover temp keeps its mode; start from a fresh 0600 file.
        let _ = fs::remove_file(&tmp);
        let mut file = crate::private::create_file(&tmp)?;
        file.write_all(text.as_bytes())?;
        // On disk before the rename, so a crash never leaves an empty or torn store.
        file.sync_all()?;
        drop(file);
        fs::rename(tmp, &self.path)
    }

    pub fn get(&self, id: &str) -> Option<&Pair> {
        self.pairs.iter().find(|p| p.id == id)
    }

    pub fn upsert(&mut self, pair: Pair) {
        match self.pairs.iter_mut().find(|p| p.id == pair.id) {
            Some(existing) => *existing = pair,
            None => self.pairs.push(pair),
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.pairs.len();
        self.pairs.retain(|p| p.id != id);
        self.pairs.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rel;
    use crate::volume::VolumeInfo;
    use std::cell::RefCell;
    use std::fs;

    /// Mount roots are plain temp folders; ids are fixed strings.
    #[derive(Default)]
    struct FakeVolumes(RefCell<Vec<VolumeInfo>>);

    impl FakeVolumes {
        fn mount(&self, id: &str, root: &Path) {
            let root = crate::safety::canonical(root).unwrap();
            self.0.borrow_mut().retain(|v| v.id != id);
            self.0.borrow_mut().push(VolumeInfo {
                id: id.into(),
                mount_root: root,
                label: format!("Disk {id}"),
            });
        }
    }

    impl Volumes for FakeVolumes {
        fn volume_of(&self, path: &Path) -> io::Result<VolumeInfo> {
            let path = crate::safety::canonical(path)?;
            self.0
                .borrow()
                .iter()
                .filter(|v| path.starts_with(&v.mount_root))
                .max_by_key(|v| v.mount_root.components().count())
                .cloned()
                .ok_or_else(|| io::Error::other("unknown drive"))
        }
        fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>> {
            Ok(self.0.borrow().iter().find(|v| v.id == id).cloned())
        }
    }

    fn setup() -> (tempfile::TempDir, FakeVolumes) {
        let t = tempfile::tempdir().unwrap();
        for d in ["diskA/Photos", "diskB/Backup/Photos"] {
            fs::create_dir_all(t.path().join(d)).unwrap();
        }
        let v = FakeVolumes::default();
        v.mount("A", &t.path().join("diskA"));
        v.mount("B", &t.path().join("diskB"));
        (t, v)
    }

    fn req<'a>(src: &'a Path, rep: &'a Path) -> NewPair<'a> {
        NewPair {
            name: " Photos ",
            source: src,
            replica: rep,
            user_rules: vec![],
            trash_days: DEFAULT_TRASH_DAYS,
            allow_same_volume: false,
        }
    }

    #[test]
    fn verify_accepts_a_pre_0_4_windows_id_for_the_same_drive() {
        let (t, v) = setup();
        v.0.borrow_mut().retain(|x| x.id != "A");
        v.mount("win:0000000089abcdef", &t.path().join("diskA"));
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        let r = resolve(&p, 0, &v).unwrap();
        // The same drive now reports the fallback serial with its own prefix.
        v.0.borrow_mut().retain(|x| x.id != "win:0000000089abcdef");
        v.mount("win32:89abcdef", &t.path().join("diskA"));
        verify_source(&p, &r, &v).unwrap();
        p.source.volume_id = "win:0000000089abcdee".into();
        assert!(matches!(
            verify_source(&p, &r, &v),
            Err(ResolveError::WrongVolume(_))
        ));
    }

    #[test]
    fn verify_source_refuses_a_different_disk() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        let r = resolve(&p, 0, &v).unwrap();
        verify_source(&p, &r, &v).unwrap();
        v.mount("Z", &t.path().join("diskA")); // another disk now answers for that path
        v.0.borrow_mut().retain(|x| x.id != "A");
        assert!(matches!(
            verify_source(&p, &r, &v),
            Err(ResolveError::WrongVolume(_))
        ));
    }

    #[test]
    fn resolve_replica_works_while_the_source_is_unplugged() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        v.0.borrow_mut().retain(|x| x.id != "A");
        let root = resolve_replica(&p, 0, &v).unwrap();
        assert!(root.ends_with("Photos"));
    }

    #[test]
    fn create_stores_drive_ids_and_paths_from_the_mount_root() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        assert_eq!(p.name, "Photos");
        assert_eq!(
            p.source,
            Side {
                volume_id: "A".into(),
                rel_path: rel("Photos"),
                label: "Disk A".into()
            }
        );
        assert_eq!(p.replicas()[0].side.rel_path.as_str(), "Backup/Photos");
        assert!(p.replicas()[0].is_first_sync());
    }

    #[test]
    fn create_validates_same_nested_same_volume_and_rules() {
        let (t, v) = setup();
        let a = t.path().join("diskA/Photos");
        assert!(matches!(
            create_pair(req(&a, &a), &v),
            Err(PairError::SameFolder)
        ));
        assert!(matches!(
            create_pair(req(&t.path().join("diskA"), &a), &v),
            Err(PairError::Nested)
        ));
        fs::create_dir_all(t.path().join("diskA/Other")).unwrap();
        let other = t.path().join("diskA/Other");
        assert!(matches!(
            create_pair(req(&a, &other), &v),
            Err(PairError::SameVolume)
        ));
        assert!(
            create_pair(
                NewPair {
                    allow_same_volume: true,
                    ..req(&a, &other)
                },
                &v
            )
            .is_ok()
        );
        let rep = t.path().join("diskB/Backup/Photos");
        let bad = NewPair {
            user_rules: vec!["[".into()],
            ..req(&a, &rep)
        };
        assert!(matches!(create_pair(bad, &v), Err(PairError::Rule(_))));
    }

    #[test]
    fn resolve_follows_the_drive_after_its_mount_point_changes() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        fs::rename(t.path().join("diskB"), t.path().join("diskB-now-F")).unwrap();
        v.mount("B", &t.path().join("diskB-now-F"));
        let r = resolve(&p, 0, &v).unwrap();
        assert_eq!(
            canonical(&r.replica_root).unwrap(),
            canonical(&t.path().join("diskB-now-F/Backup/Photos")).unwrap()
        );
        verify_replica(&p, 0, &r, &v).unwrap();
    }

    #[test]
    fn resolve_reports_missing_drive_and_missing_folder() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        v.0.borrow_mut().retain(|x| x.id != "B");
        assert!(matches!(
            resolve(&p, 0, &v),
            Err(ResolveError::NotConnected {
                side: SideKind::Replica,
                ..
            })
        ));
        v.mount("B", &t.path().join("diskB"));
        fs::remove_dir_all(t.path().join("diskB/Backup")).unwrap();
        assert!(matches!(
            resolve(&p, 0, &v),
            Err(ResolveError::FolderMissing(_))
        ));
    }

    #[test]
    fn verify_refuses_a_different_disk_on_the_same_path() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        let r = resolve(&p, 0, &v).unwrap();
        v.mount("C", &t.path().join("diskB")); // same place, different disk
        v.0.borrow_mut().retain(|x| x.id != "B");
        assert!(matches!(
            verify_replica(&p, 0, &r, &v),
            Err(ResolveError::WrongVolume(_))
        ));
    }

    #[test]
    fn relink_replaces_the_side_and_makes_the_next_sync_a_first_sync() {
        let (t, v) = setup();
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        p.replica_mut(0).unwrap().last_sync = Some(LastSync {
            at: "2026-10-01T10:00:00Z".into(),
            applied: 3,
            failed: 0,
            stopped: false,
        });
        fs::create_dir_all(t.path().join("diskC/Photos")).unwrap();
        v.mount("C", &t.path().join("diskC"));
        relink_replica(&mut p, 0, &t.path().join("diskC/Photos"), false, &v).unwrap();
        assert_eq!(p.replicas()[0].side.volume_id, "C");
        assert!(p.replicas()[0].is_first_sync());
    }

    /// Fails `find` for one drive id, as a drive that can't be queried does.
    struct FindFails<'a>(&'a FakeVolumes, &'static str);

    impl Volumes for FindFails<'_> {
        fn volume_of(&self, path: &Path) -> io::Result<VolumeInfo> {
            self.0.volume_of(path)
        }
        fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>> {
            if id == self.1 {
                return Err(io::Error::other("drive query failed"));
            }
            self.0.find(id)
        }
    }

    #[test]
    fn relink_reports_an_io_error_on_the_other_side_instead_of_skipping_the_check() {
        let (t, v) = setup();
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        fs::create_dir_all(t.path().join("diskC/Photos")).unwrap();
        v.mount("C", &t.path().join("diskC"));
        let before = p.clone();
        let r = relink_replica(
            &mut p,
            0,
            &t.path().join("diskC/Photos"),
            false,
            &FindFails(&v, "A"),
        );
        assert!(matches!(r, Err(PairError::Io(_))), "{r:?}");
        assert_eq!(p, before);
    }

    #[test]
    fn relink_skips_the_nested_check_while_the_other_side_is_away() {
        let (t, v) = setup();
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        v.0.borrow_mut().retain(|x| x.id != "A");
        fs::create_dir_all(t.path().join("diskC/Photos")).unwrap();
        v.mount("C", &t.path().join("diskC"));
        relink_replica(&mut p, 0, &t.path().join("diskC/Photos"), false, &v).unwrap();
        assert_eq!(p.replicas()[0].side.volume_id, "C");
    }

    #[test]
    fn relinking_onto_the_other_sides_drive_asks_first() {
        let (t, v) = setup();
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        fs::create_dir_all(t.path().join("diskA/Backup")).unwrap();
        let onto_a = t.path().join("diskA/Backup");
        let r = relink_replica(&mut p, 0, &onto_a, false, &v);
        assert!(matches!(r, Err(PairError::SameVolume)), "{r:?}");
        assert_eq!(p.replicas()[0].side.volume_id, "B");
        relink_replica(&mut p, 0, &onto_a, true, &v).unwrap();
        assert_eq!(p.replicas()[0].side.volume_id, "A");
    }

    #[test]
    fn a_folder_deleted_on_a_connected_drive_is_missing_not_disconnected() {
        let (t, v) = setup();
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        let resolved = resolve(&p, 0, &v).unwrap();
        fs::remove_dir(t.path().join("diskB/Backup/Photos")).unwrap();
        let r = verify_replica(&p, 0, &resolved, &v);
        assert!(matches!(r, Err(ResolveError::FolderMissing(_))), "{r:?}");
        v.0.borrow_mut().retain(|x| x.id != "B");
        let r = verify_replica(&p, 0, &resolved, &v);
        assert!(matches!(r, Err(ResolveError::NotConnected { .. })), "{r:?}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_folder_name_that_is_not_unicode_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let (t, v) = setup();
        let bad = t
            .path()
            .join("diskB")
            .join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        fs::create_dir(&bad).unwrap();
        let r = create_pair(req(&t.path().join("diskA/Photos"), &bad), &v);
        let Err(PairError::Io(e)) = r else {
            panic!("{r:?}");
        };
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn pair_ids_never_repeat() {
        let ids: std::collections::HashSet<String> = (0..1000).map(|_| new_id()).collect();
        assert_eq!(ids.len(), 1000);
    }

    #[test]
    fn last_sync_from_before_stopped_existed_reads_as_not_stopped() {
        let old: LastSync =
            serde_json::from_str(r#"{"at":"2026-10-01T10:00:00Z","applied":1,"failed":0}"#)
                .unwrap();
        assert!(!old.stopped);
    }

    #[test]
    fn store_round_trips_and_missing_file_is_empty() {
        let (t, v) = setup();
        let path = t.path().join("data/pairs.json");
        let mut store = PairStore::load(&path).unwrap();
        assert!(store.pairs.is_empty());
        let p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        store.upsert(p.clone());
        store.save().unwrap();
        assert!(!path.with_extension("json.tmp").exists());
        let again = PairStore::load(&path).unwrap();
        assert_eq!(again.get(&p.id), Some(&p));
        let mut again = again;
        assert!(again.remove(&p.id));
        assert!(again.get(&p.id).is_none());
    }

    /// A store as 0.4 wrote it (format 1: one `replica`, history on the pair).
    const STORE_0_4: &str = r#"{
  "version": 1,
  "pairs": [
    {
      "id": "18a2b3c4d5e6f708",
      "name": "Photos",
      "source": {
        "volume_id": "uuid:1111-AAAA",
        "rel_path": "Users/me/Photos",
        "label": "Disk A"
      },
      "replica": {
        "volume_id": "win32:0a1b2c3d",
        "rel_path": "",
        "label": "STICK"
      },
      "user_rules": [
        "*.tmp"
      ],
      "trash_days": 30,
      "last_sync": {
        "at": "2026-10-17T15:53:45Z",
        "applied": 3,
        "failed": 0,
        "stopped": false
      },
      "last_scan_files": 120
    }
  ]
}"#;

    /// The same store as this build writes it: format 2, the disk's history
    /// moved into its `replicas` entry.
    const STORE_0_4_AS_V2: &str = r#"{
  "version": 2,
  "pairs": [
    {
      "id": "18a2b3c4d5e6f708",
      "name": "Photos",
      "source": {
        "volume_id": "uuid:1111-AAAA",
        "rel_path": "Users/me/Photos",
        "label": "Disk A"
      },
      "replicas": [
        {
          "side": {
            "volume_id": "win32:0a1b2c3d",
            "rel_path": "",
            "label": "STICK"
          },
          "last_sync": {
            "at": "2026-10-17T15:53:45Z",
            "applied": 3,
            "failed": 0,
            "stopped": false
          },
          "last_scan_files": 120
        }
      ],
      "user_rules": [
        "*.tmp"
      ],
      "trash_days": 30
    }
  ]
}"#;

    #[test]
    fn a_0_4_store_loads_untouched_and_saves_as_format_2() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pairs.json");
        fs::write(&path, STORE_0_4).unwrap();
        let store = PairStore::load(&path).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            STORE_0_4,
            "loading never writes"
        );
        assert!(!path.with_extension("json.tmp").exists());
        let p = &store.pairs[0];
        assert_eq!(p.source.rel_path, rel("Users/me/Photos"));
        assert_eq!(p.replicas().len(), 1);
        assert!(p.replicas()[0].side.rel_path.is_root());
        // The pair's own history lands on its one disk.
        assert_eq!(p.replicas()[0].last_sync.as_ref().unwrap().applied, 3);
        assert_eq!(p.replicas()[0].last_scan_files, Some(120));
        store.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), STORE_0_4_AS_V2);
        assert_eq!(PairStore::load(&path).unwrap().pairs, store.pairs);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn a_store_from_a_newer_format_is_refused_and_one_without_a_version_loads() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pairs.json");
        fs::write(
            &path,
            STORE_0_4_AS_V2.replace(r#""version": 2"#, r#""version": 3"#),
        )
        .unwrap();
        let e = PairStore::load(&path).unwrap_err();
        assert!(e.to_string().contains("newer replica-sync"), "{e}");
        fs::write(&path, STORE_0_4.replace("  \"version\": 1,\n", "")).unwrap();
        assert_eq!(PairStore::load(&path).unwrap().pairs.len(), 1);
        fs::write(&path, STORE_0_4_AS_V2).unwrap();
        assert_eq!(PairStore::load(&path).unwrap().pairs.len(), 1);
    }

    #[test]
    fn a_stored_path_that_leaves_its_folder_fails_to_load() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pairs.json");
        for bad in ["a/../b", "..", "a//b", "./a"] {
            fs::write(&path, STORE_0_4.replace("Users/me/Photos", bad)).unwrap();
            let Err(e) = PairStore::load(&path) else {
                panic!("{bad} loaded");
            };
            assert!(
                e.to_string().contains("invalid relative path"),
                "{bad}: {e}"
            );
        }
    }

    fn synced(at: &str) -> Option<LastSync> {
        Some(LastSync {
            at: at.into(),
            applied: 1,
            failed: 0,
            stopped: false,
        })
    }

    /// Source on A; disk B and a third disk C each hold a backup folder.
    fn two_replicas() -> (tempfile::TempDir, FakeVolumes, Pair) {
        let (t, v) = setup();
        fs::create_dir_all(t.path().join("diskC/Backup/Photos")).unwrap();
        v.mount("C", &t.path().join("diskC"));
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        add_replica(&mut p, &t.path().join("diskC/Backup/Photos"), false, &v).unwrap();
        (t, v, p)
    }

    #[test]
    fn a_pair_takes_a_second_backup_disk_and_no_third() {
        let (t, v, mut p) = two_replicas();
        assert_eq!(p.replicas().len(), 2);
        assert_eq!(p.replicas()[1].side.volume_id, "C");
        assert!(p.replicas()[1].is_first_sync());
        fs::create_dir_all(t.path().join("diskD/Backup")).unwrap();
        v.mount("D", &t.path().join("diskD"));
        let r = add_replica(&mut p, &t.path().join("diskD/Backup"), false, &v);
        assert!(matches!(r, Err(PairError::TooManyReplicas)), "{r:?}");
        assert_eq!(p.replicas().len(), MAX_REPLICAS);
    }

    #[test]
    fn a_backup_disk_is_checked_like_the_first_one() {
        let (t, v) = setup();
        let mut p = create_pair(
            req(
                &t.path().join("diskA/Photos"),
                &t.path().join("diskB/Backup/Photos"),
            ),
            &v,
        )
        .unwrap();
        let before = p.clone();
        // The first backup folder again, nested inside it, and on the source's drive.
        fs::create_dir_all(t.path().join("diskB/Backup/Photos/Deep")).unwrap();
        fs::create_dir_all(t.path().join("diskA/Backup")).unwrap();
        for (folder, want) in [
            ("diskB/Backup/Photos", "SameFolder"),
            ("diskB/Backup/Photos/Deep", "Nested"),
            ("diskB/Backup", "Nested"),
            ("diskA/Backup", "SameVolume"),
        ] {
            let r = add_replica(&mut p, &t.path().join(folder), false, &v);
            assert!(format!("{r:?}").contains(want), "{folder}: {r:?}");
            assert_eq!(p, before, "{folder} changed the pair");
        }
        add_replica(&mut p, &t.path().join("diskA/Backup"), true, &v).unwrap();
        assert_eq!(p.replicas().len(), 2);
    }

    #[test]
    fn a_replica_can_only_be_removed_while_another_remains() {
        let (_t, _v, mut p) = two_replicas();
        p.replica_mut(0).unwrap().last_sync = synced("2026-10-01T10:00:00Z");
        p.replica_mut(1).unwrap().last_sync = synced("2026-10-02T10:00:00Z");
        assert!(matches!(
            remove_replica(&mut p, 2),
            Err(PairError::NoSuchReplica(2))
        ));
        remove_replica(&mut p, 0).unwrap();
        assert_eq!(p.replicas().len(), 1);
        assert_eq!(p.replicas()[0].side.volume_id, "C");
        assert_eq!(
            p.replicas()[0].last_sync.as_ref().unwrap().at,
            "2026-10-02T10:00:00Z",
            "the remaining disk keeps its own history"
        );
        let before = p.clone();
        assert!(matches!(
            remove_replica(&mut p, 0),
            Err(PairError::LastReplica)
        ));
        assert_eq!(p, before);
    }

    #[test]
    fn a_last_sync_with_an_unreadable_time_is_still_a_first_sync() {
        let (_t, _v, mut p) = two_replicas();
        p.replica_mut(0).unwrap().last_sync = synced("not a time");
        assert!(p.replicas()[0].is_first_sync());
        p.replica_mut(0).unwrap().last_sync = synced("2026-10-01T10:00:00+00:00");
        assert!(!p.replicas()[0].is_first_sync());
    }

    #[test]
    fn the_first_sync_guard_is_per_disk() {
        let (_t, _v, mut p) = two_replicas();
        p.replica_mut(0).unwrap().last_sync = synced("2026-10-01T10:00:00Z");
        assert!(!p.replicas()[0].is_first_sync());
        assert!(
            p.replicas()[1].is_first_sync(),
            "disk B has never been synced to, whatever disk A has"
        );
    }

    #[test]
    fn relinking_one_disk_drops_only_its_history() {
        let (t, v, mut p) = two_replicas();
        for (i, at) in ["2026-10-01T10:00:00Z", "2026-10-02T10:00:00Z"]
            .iter()
            .enumerate()
        {
            let r = p.replica_mut(i).unwrap();
            r.last_sync = synced(at);
            r.last_scan_files = Some(10);
        }
        fs::create_dir_all(t.path().join("diskD/Backup")).unwrap();
        v.mount("D", &t.path().join("diskD"));
        relink_replica(&mut p, 1, &t.path().join("diskD/Backup"), false, &v).unwrap();
        assert_eq!(p.replicas()[1].side.volume_id, "D");
        assert!(p.replicas()[1].is_first_sync());
        assert_eq!(p.replicas()[1].last_scan_files, None);
        assert!(!p.replicas()[0].is_first_sync(), "disk A's history stays");
        assert_eq!(p.replicas()[0].last_scan_files, Some(10));
        let r = relink_replica(&mut p, 5, &t.path().join("diskD/Backup"), false, &v);
        assert!(matches!(r, Err(PairError::NoSuchReplica(5))), "{r:?}");
    }

    #[test]
    fn relinking_the_source_makes_every_disk_a_first_sync_again() {
        let (t, v, mut p) = two_replicas();
        for i in 0..2 {
            let r = p.replica_mut(i).unwrap();
            r.last_sync = synced("2026-10-01T10:00:00Z");
            r.last_scan_files = Some(10);
        }
        fs::create_dir_all(t.path().join("diskD/Photos")).unwrap();
        v.mount("D", &t.path().join("diskD"));
        relink_source(&mut p, &t.path().join("diskD/Photos"), false, &v).unwrap();
        assert_eq!(p.source.volume_id, "D");
        assert!(p.replicas().iter().all(|r| r.is_first_sync()));
        assert!(p.replicas().iter().all(|r| r.last_scan_files.is_none()));
        // And a source on a backup disk's drive asks first.
        fs::create_dir_all(t.path().join("diskB/Other")).unwrap();
        let r = relink_source(&mut p, &t.path().join("diskB/Other"), false, &v);
        assert!(matches!(r, Err(PairError::SameVolume)), "{r:?}");
        assert_eq!(p.source.volume_id, "D");
    }

    #[test]
    fn resolve_picks_the_backup_disk_asked_for() {
        let (t, v, p) = two_replicas();
        let b = resolve(&p, 0, &v).unwrap();
        let c = resolve(&p, 1, &v).unwrap();
        assert_eq!(b.source_root, c.source_root);
        assert!(
            b.replica_root
                .starts_with(canonical(&t.path().join("diskB")).unwrap())
        );
        assert!(
            c.replica_root
                .starts_with(canonical(&t.path().join("diskC")).unwrap())
        );
        verify_replica(&p, 1, &c, &v).unwrap();
        // Disk B's folder is not disk C's.
        assert!(matches!(
            verify_replica(&p, 1, &b, &v),
            Err(ResolveError::WrongVolume(_))
        ));
        assert!(matches!(
            resolve(&p, 2, &v),
            Err(ResolveError::NoSuchReplica(2))
        ));
        assert!(matches!(
            resolve_replica(&p, 2, &v),
            Err(ResolveError::NoSuchReplica(2))
        ));
        v.0.borrow_mut().retain(|x| x.id != "B");
        assert!(resolve(&p, 0, &v).is_err(), "disk B is away");
        assert!(resolve(&p, 1, &v).is_ok(), "disk C is not");
    }

    #[test]
    fn two_replicas_round_trip_byte_for_byte() {
        let (t, _v, mut p) = two_replicas();
        p.replica_mut(0).unwrap().last_sync = synced("2026-10-01T10:00:00Z");
        let path = t.path().join("pairs.json");
        let mut store = PairStore::load(&path).unwrap();
        store.upsert(p.clone());
        store.save().unwrap();
        let written = fs::read_to_string(&path).unwrap();
        let again = PairStore::load(&path).unwrap();
        assert_eq!(again.pairs, vec![p]);
        again.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), written);
        assert!(written.contains(r#""version": 2"#));
    }

    /// The first pair of `STORE_0_4_AS_V2` with its `replicas` value replaced.
    fn v2_with_replicas(replicas: &str) -> String {
        let a = STORE_0_4_AS_V2.find("\"replicas\"").unwrap();
        let b = STORE_0_4_AS_V2.find("\"user_rules\"").unwrap();
        format!(
            "{}\"replicas\": {replicas},\n      {}",
            &STORE_0_4_AS_V2[..a],
            &STORE_0_4_AS_V2[b..]
        )
    }

    fn load_text(text: &str) -> io::Result<PairStore> {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("pairs.json");
        fs::write(&path, text).unwrap();
        PairStore::load(&path)
    }

    #[test]
    fn a_store_must_hold_one_or_two_replicas_per_pair() {
        let one = r#"{"side":{"volume_id":"X","rel_path":"","label":"X"},"last_sync":null,"last_scan_files":null}"#;
        let list = |n: usize| format!("[{}]", vec![one; n].join(","));
        assert!(load_text(&v2_with_replicas(&list(1))).is_ok());
        assert!(load_text(&v2_with_replicas(&list(2))).is_ok());
        for n in [0, 3] {
            let e = load_text(&v2_with_replicas(&list(n))).unwrap_err();
            assert!(e.to_string().contains("expected 1 to 2"), "{n}: {e}");
        }
    }

    #[test]
    fn a_pair_with_both_shapes_or_neither_is_refused_not_guessed_at() {
        // Both the old `replica` and `replicas`.
        let both = STORE_0_4_AS_V2.replace(
            r#""user_rules""#,
            r#""replica": {"volume_id": "Z", "rel_path": "", "label": "Z"}, "user_rules""#,
        );
        let e = load_text(&both).unwrap_err();
        assert!(
            e.to_string().contains("both `replica` and `replicas`"),
            "{e}"
        );
        // Neither.
        let e = load_text(&v2_with_replicas("null")).unwrap_err();
        assert!(e.to_string().contains("has no replica"), "{e}");
        // `replicas` next to a pair-level history, which would be lost silently.
        let mixed =
            STORE_0_4_AS_V2.replace(r#""user_rules""#, r#""last_scan_files": 9, "user_rules""#);
        let e = load_text(&mixed).unwrap_err();
        assert!(e.to_string().contains("pair-level"), "{e}");
    }
}
