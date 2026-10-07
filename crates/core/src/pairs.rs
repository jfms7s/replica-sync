//! Saved source → replica pairs, found by drive ID rather than drive letter.

use crate::rules::{RuleError, SkipRules};
use crate::volume::Volumes;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_TRASH_DAYS: u32 = 30;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Side {
    pub volume_id: String,
    /// `/`-separated path from the drive's mount root; empty for the root itself.
    pub rel_path: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastSync {
    /// RFC 3339.
    pub at: String,
    pub applied: usize,
    pub failed: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    pub id: String,
    pub name: String,
    pub source: Side,
    pub replica: Side,
    pub user_rules: Vec<String>,
    pub trash_days: u32,
    pub last_sync: Option<LastSync>,
    /// Files seen in the last source scan, for an approximate progress bar.
    pub last_scan_files: Option<u64>,
}

impl Pair {
    pub fn rules(&self) -> Result<SkipRules, RuleError> {
        SkipRules::new(&self.user_rules)
    }

    /// The wrong-folder guard applies only to a pair's first sync.
    pub fn is_first_sync(&self) -> bool {
        self.last_sync.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SideKind {
    Source,
    Replica,
}

#[derive(Debug, thiserror::Error)]
pub enum PairError {
    #[error("the source and the replica are the same folder")]
    SameFolder,
    #[error("one folder is inside the other")]
    Nested,
    #[error("the source and the replica are on the same drive; confirm to continue")]
    SameVolume,
    #[error(transparent)]
    Rule(#[from] RuleError),
    #[error("no pair with id {0}")]
    NotFound(String),
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

fn side_for(path: &Path, volumes: &dyn Volumes) -> Result<(Side, PathBuf), PairError> {
    let canonical = dunce::canonicalize(path)?;
    let vol = volumes.volume_of(&canonical)?;
    let rest = canonical
        .strip_prefix(&vol.mount_root)
        .map_err(|_| io::Error::other("folder is not under its drive's mount point"))?;
    let rel_path = rest
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Ok((
        Side {
            volume_id: vol.id,
            rel_path,
            label: vol.label,
        },
        canonical,
    ))
}

fn check_folders(a: &Path, b: &Path) -> Result<(), PairError> {
    if a == b {
        return Err(PairError::SameFolder);
    }
    if a.starts_with(b) || b.starts_with(a) {
        return Err(PairError::Nested);
    }
    Ok(())
}

fn new_id() -> String {
    format!(
        "{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    )
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
        replica,
        user_rules: req.user_rules,
        trash_days: req.trash_days,
        last_sync: None,
        last_scan_files: None,
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
    let mut root = vol.mount_root;
    for part in side.rel_path.split('/').filter(|p| !p.is_empty()) {
        if part == ".." || part == "." {
            return Err(ResolveError::FolderMissing(root));
        }
        root.push(part);
    }
    if !root.is_dir() {
        return Err(ResolveError::FolderMissing(root));
    }
    Ok(root)
}

pub fn resolve(pair: &Pair, volumes: &dyn Volumes) -> Result<Resolved, ResolveError> {
    Ok(Resolved {
        source_root: resolve_side(&pair.source, SideKind::Source, volumes)?,
        replica_root: resolve_side(&pair.replica, SideKind::Replica, volumes)?,
    })
}

/// Re-checked right before Apply: a different disk on the same letter is refused.
pub fn verify_replica(
    pair: &Pair,
    resolved: &Resolved,
    volumes: &dyn Volumes,
) -> Result<(), ResolveError> {
    match volumes.volume_of(&resolved.replica_root) {
        Ok(v) if v.id == pair.replica.volume_id => Ok(()),
        Ok(_) => Err(ResolveError::WrongVolume(resolved.replica_root.clone())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Err(ResolveError::NotConnected {
            side: SideKind::Replica,
            label: pair.replica.label.clone(),
        }),
        Err(e) => Err(e.into()),
    }
}

pub fn relink(
    pair: &mut Pair,
    side: SideKind,
    folder: &Path,
    volumes: &dyn Volumes,
) -> Result<(), PairError> {
    let (new_side, new_path) = side_for(folder, volumes)?;
    let other = match side {
        SideKind::Source => (&pair.replica, SideKind::Replica),
        SideKind::Replica => (&pair.source, SideKind::Source),
    };
    if let Ok(other_path) = resolve_side(other.0, other.1, volumes) {
        check_folders(&new_path, &dunce::canonicalize(other_path)?)?;
    }
    match side {
        SideKind::Source => pair.source = new_side,
        SideKind::Replica => pair.replica = new_side,
    }
    pair.last_sync = None;
    pair.last_scan_files = None;
    Ok(())
}

#[derive(Serialize)]
struct StoreOut<'a> {
    version: u32,
    pairs: &'a [Pair],
}

#[derive(Deserialize)]
struct StoreIn {
    pairs: Vec<Pair>,
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
                serde_json::from_str::<StoreIn>(&text)
                    .map_err(io::Error::other)?
                    .pairs
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
            fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(&StoreOut {
            version: 1,
            pairs: &self.pairs,
        })
        .map_err(io::Error::other)?;
        fs::write(&tmp, text)?;
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
    use crate::volume::VolumeInfo;
    use std::cell::RefCell;
    use std::fs;

    /// Mount roots are plain temp folders; ids are fixed strings.
    #[derive(Default)]
    struct FakeVolumes(RefCell<Vec<VolumeInfo>>);

    impl FakeVolumes {
        fn mount(&self, id: &str, root: &Path) {
            let root = dunce::canonicalize(root).unwrap();
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
            let path = dunce::canonicalize(path)?;
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
                rel_path: "Photos".into(),
                label: "Disk A".into()
            }
        );
        assert_eq!(p.replica.rel_path, "Backup/Photos");
        assert!(p.is_first_sync());
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
        let r = resolve(&p, &v).unwrap();
        assert_eq!(
            dunce::canonicalize(&r.replica_root).unwrap(),
            dunce::canonicalize(t.path().join("diskB-now-F/Backup/Photos")).unwrap()
        );
        verify_replica(&p, &r, &v).unwrap();
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
            resolve(&p, &v),
            Err(ResolveError::NotConnected {
                side: SideKind::Replica,
                ..
            })
        ));
        v.mount("B", &t.path().join("diskB"));
        fs::remove_dir_all(t.path().join("diskB/Backup")).unwrap();
        assert!(matches!(
            resolve(&p, &v),
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
        let r = resolve(&p, &v).unwrap();
        v.mount("C", &t.path().join("diskB")); // same place, different disk
        v.0.borrow_mut().retain(|x| x.id != "B");
        assert!(matches!(
            verify_replica(&p, &r, &v),
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
        p.last_sync = Some(LastSync {
            at: "2026-10-01T10:00:00Z".into(),
            applied: 3,
            failed: 0,
        });
        fs::create_dir_all(t.path().join("diskC/Photos")).unwrap();
        v.mount("C", &t.path().join("diskC"));
        relink(
            &mut p,
            SideKind::Replica,
            &t.path().join("diskC/Photos"),
            &v,
        )
        .unwrap();
        assert_eq!(p.replica.volume_id, "C");
        assert!(p.is_first_sync());
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
        let again = PairStore::load(&path).unwrap();
        assert_eq!(again.get(&p.id), Some(&p));
        let mut again = again;
        assert!(again.remove(&p.id));
        assert!(again.get(&p.id).is_none());
    }
}
