//! Every write, rename or trash target must resolve inside the replica folder.
//!
//! [`ReplicaRoot`] is the only write path into the replica: its methods check
//! containment and act in one call, so a new caller cannot skip the check.
//! (Paths it hands out are for reading metadata, and for the temps a run
//! created itself.)

use crate::model::RelPath;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// How two folders relate, for the "same folder or one inside the other" rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nesting {
    Same,
    Nested,
}

/// Whether `a` and `b` are the same folder or one is inside the other. Both
/// must already be in the same canonical form; this compares components.
pub fn nesting(a: &Path, b: &Path) -> Option<Nesting> {
    if a == b {
        Some(Nesting::Same)
    } else if a.starts_with(b) || b.starts_with(a) {
        Some(Nesting::Nested)
    } else {
        None
    }
}

/// The canonical form every path comparison in replica-sync uses: a thin
/// wrapper over `std::fs::canonicalize`, which is always `\\?\` verbatim on
/// Windows. Never compare with `dunce::canonicalize`: it drops the verbatim
/// prefix only when it can (below 260 characters, and not for reserved names,
/// trailing dots or spaces), so two of its results (or one of its and one of
/// these) stop comparing once a path gets long or unusual. `dunce`
/// is for display and Win32 calls only (clippy's `disallowed-methods` enforces
/// this; see `clippy.toml`).
pub fn canonical(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path)
}

/// `rel` with `suffix` added to its last name (a temp or rename-aside name).
pub fn with_suffix(rel: &RelPath, suffix: &str) -> RelPath {
    RelPath::new(&format!("{}{suffix}", rel.as_str())).expect("a suffix keeps a valid path valid")
}

#[derive(Debug, thiserror::Error)]
pub enum SafetyError {
    #[error("{} is outside the replica folder", .0.display())]
    Escapes(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug)]
pub struct ReplicaRoot {
    root: PathBuf,
    /// From [`canonical`], so it compares with the probes in `target`
    /// whatever their length.
    canonical: PathBuf,
}

impl ReplicaRoot {
    pub fn new(root: &Path) -> io::Result<ReplicaRoot> {
        Ok(ReplicaRoot {
            root: root.to_path_buf(),
            canonical: canonical(root)?,
        })
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// The absolute path for `rel`, after checking that its deepest existing
    /// ancestor resolves (through any links) inside the replica folder.
    pub fn target(&self, rel: &RelPath) -> Result<PathBuf, SafetyError> {
        let target = rel.to_path(&self.root);
        let mut probe = target.parent();
        while let Some(dir) = probe {
            match canonical(dir) {
                Ok(real) if real.starts_with(&self.canonical) => return Ok(target),
                Ok(_) => return Err(SafetyError::Escapes(target)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    // Something there that doesn't resolve (a dangling link) has
                    // escaped; only a path that isn't there walks up a level.
                    match fs::symlink_metadata(dir) {
                        Ok(_) => return Err(SafetyError::Escapes(target)),
                        Err(e) if e.kind() == io::ErrorKind::NotFound => probe = dir.parent(),
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(SafetyError::Escapes(target))
    }

    /// Creates the folder `rel` and any missing parents.
    pub fn create_dir_all(&self, rel: &RelPath) -> Result<(), SafetyError> {
        Ok(fs::create_dir_all(self.target(rel)?)?)
    }

    /// Creates the parent folders of `rel` (never the root) and returns its path.
    pub fn create_parent(&self, rel: &RelPath) -> Result<PathBuf, SafetyError> {
        // Checking `rel` checks its deepest existing ancestor, which is also
        // the parent's: one check covers both.
        let target = self.target(rel)?;
        fs::create_dir_all(target.parent().expect("never the root"))?;
        Ok(target)
    }

    /// Creates `rel` as a new file for writing; fails `AlreadyExists` when
    /// anything (a link included) is already there.
    pub fn create_new(&self, rel: &RelPath) -> Result<(PathBuf, File), SafetyError> {
        let path = self.target(rel)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok((path, file))
    }

    /// Renames `from` to `to`, both inside the replica.
    pub fn rename(&self, from: &RelPath, to: &RelPath) -> Result<(), SafetyError> {
        let from_path = self.target(from)?;
        // Same folder (a temp or a rename aside): one check covers both.
        let to_path = if from.parent() == to.parent() {
            to.to_path(&self.root)
        } else {
            self.target(to)?
        };
        Ok(fs::rename(from_path, to_path)?)
    }

    /// Removes the empty folder `rel`.
    pub fn remove_dir(&self, rel: &RelPath) -> Result<(), SafetyError> {
        Ok(fs::remove_dir(self.target(rel)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::rel;

    #[test]
    fn targets_inside_the_replica_are_allowed_even_if_missing() {
        let d = tempfile::tempdir().unwrap();
        let r = ReplicaRoot::new(d.path()).unwrap();
        assert_eq!(
            r.target(&rel("a/b/c.txt")).unwrap(),
            d.path().join("a").join("b").join("c.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_parent_pointing_outside_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("Docs")).unwrap();
        let r = ReplicaRoot::new(d.path()).unwrap();
        assert!(matches!(
            r.target(&rel("Docs/x.txt")),
            Err(SafetyError::Escapes(_))
        ));
    }

    /// An ancestor that can't be searched is an error, never "not there".
    #[cfg(unix)]
    #[test]
    fn an_unreadable_ancestor_is_an_io_error() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let locked = d.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let root_user = std::fs::read_dir(&locked).is_ok(); // root searches any folder
        let r = ReplicaRoot::new(d.path()).unwrap();
        let got = r.target(&rel("locked/sub/x.txt"));
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if root_user {
            return;
        }
        match got {
            Err(SafetyError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::PermissionDenied),
            other => panic!("{other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_symlinked_parent_is_refused() {
        let d = tempfile::tempdir().unwrap();
        // Create a symlink to a non-existent location outside the replica
        std::os::unix::fs::symlink("/outside/not-yet-created", d.path().join("Docs")).unwrap();
        let r = ReplicaRoot::new(d.path()).unwrap();
        // The dangling symlink should be caught and refused
        assert!(matches!(
            r.target(&rel("Docs/x.txt")),
            Err(SafetyError::Escapes(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn junctioned_parent_pointing_outside_is_refused() {
        let outside = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        crate::testutil::junction(&d.path().join("Docs"), outside.path());
        let r = ReplicaRoot::new(d.path()).unwrap();
        assert!(matches!(
            r.target(&rel("Docs/x.txt")),
            Err(SafetyError::Escapes(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn junction_that_stays_inside_is_allowed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        crate::testutil::junction(&d.path().join("alias"), &d.path().join("real"));
        let r = ReplicaRoot::new(d.path()).unwrap();
        assert_eq!(
            r.target(&rel("alias/x.txt")).unwrap(),
            d.path().join("alias").join("x.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_that_stays_inside_is_allowed() {
        let d = tempfile::tempdir().unwrap();
        // Create a real directory inside the replica
        std::fs::create_dir(d.path().join("real")).unwrap();
        // Create a symlink inside the replica pointing to the real directory
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("alias")).unwrap();
        let r = ReplicaRoot::new(d.path()).unwrap();
        // The symlink that stays inside should be allowed
        assert_eq!(
            r.target(&rel("alias/x.txt")).unwrap(),
            d.path().join("alias").join("x.txt")
        );
    }

    #[test]
    fn nesting_tells_same_from_nested() {
        let p = Path::new;
        assert_eq!(nesting(p("/a/b"), p("/a/b")), Some(Nesting::Same));
        assert_eq!(nesting(p("/a/b"), p("/a/b/c")), Some(Nesting::Nested));
        assert_eq!(nesting(p("/a/b/c"), p("/a/b")), Some(Nesting::Nested));
        assert_eq!(nesting(p("/a/b"), p("/a/bc")), None);
    }

    #[test]
    fn actions_inside_the_replica_work() {
        let d = tempfile::tempdir().unwrap();
        let r = ReplicaRoot::new(d.path()).unwrap();
        r.create_dir_all(&rel("a/b")).unwrap();
        let dest = r.create_parent(&rel("c/d/x.txt")).unwrap();
        assert_eq!(dest, d.path().join("c").join("d").join("x.txt"));
        let tmp = with_suffix(&rel("c/d/x.txt"), ".tmp");
        let (path, _) = r.create_new(&tmp).unwrap();
        assert_eq!(path, d.path().join("c").join("d").join("x.txt.tmp"));
        assert_eq!(
            r.create_new(&tmp).err().map(|e| match e {
                SafetyError::Io(e) => e.kind(),
                SafetyError::Escapes(_) => io::ErrorKind::Other,
            }),
            Some(io::ErrorKind::AlreadyExists)
        );
        r.rename(&tmp, &rel("c/d/x.txt")).unwrap();
        assert!(dest.is_file());
        r.remove_dir(&rel("a/b")).unwrap();
        assert!(!d.path().join("a/b").exists());
    }

    #[cfg(unix)]
    #[test]
    fn actions_under_a_link_pointing_outside_are_refused_and_touch_nothing() {
        let outside = tempfile::tempdir().unwrap();
        let d = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.txt"), b"keep").unwrap();
        std::fs::create_dir(outside.path().join("empty")).unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("Docs")).unwrap();
        std::fs::write(d.path().join("in.txt"), b"in").unwrap();
        let r = ReplicaRoot::new(d.path()).unwrap();
        let escapes = |e: SafetyError| matches!(e, SafetyError::Escapes(_));
        assert!(escapes(r.create_dir_all(&rel("Docs/new")).unwrap_err()));
        assert!(escapes(
            r.create_parent(&rel("Docs/new/x.txt")).unwrap_err()
        ));
        assert!(escapes(r.create_new(&rel("Docs/y.txt")).unwrap_err()));
        assert!(escapes(
            r.rename(&rel("in.txt"), &rel("Docs/in.txt")).unwrap_err()
        ));
        assert!(escapes(
            r.rename(&rel("Docs/x.txt"), &rel("x.txt")).unwrap_err()
        ));
        assert!(escapes(r.remove_dir(&rel("Docs/empty")).unwrap_err()));
        let mut left: Vec<_> = std::fs::read_dir(outside.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["empty", "x.txt"]);
        assert_eq!(
            std::fs::read(outside.path().join("x.txt")).unwrap(),
            b"keep"
        );
        assert!(d.path().join("in.txt").exists());
    }
}
