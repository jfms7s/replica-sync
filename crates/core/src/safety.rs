//! Every write, rename or trash target must resolve inside the replica folder.

use crate::model::RelPath;
use std::io;
use std::path::{Path, PathBuf};

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
    canonical: PathBuf,
}

impl ReplicaRoot {
    pub fn new(root: &Path) -> io::Result<ReplicaRoot> {
        Ok(ReplicaRoot {
            root: root.to_path_buf(),
            canonical: dunce::canonicalize(root)?,
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
            match dunce::canonicalize(dir) {
                Ok(real) if real.starts_with(&self.canonical) => return Ok(target),
                Ok(_) => return Err(SafetyError::Escapes(target)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => probe = dir.parent(),
                Err(e) => return Err(e.into()),
            }
        }
        Err(SafetyError::Escapes(target))
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
}
