//! The app's own files (pair store, settings, run logs): private to the user
//! on Unix (folders 0700, files 0600). Other systems use their defaults.

use std::fs::{self, File};
use std::io;
use std::path::Path;

/// Creates `dir` and any missing parents; `dir` itself is made 0700 even if
/// it already existed (an older version made it with the default umask).
pub fn create_dir_all(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let md = fs::metadata(dir)?;
        if md.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(dir)
    }
}

/// Creates (or truncates) `path` for writing; a new file is 0600.
pub fn create_file(path: &Path) -> io::Result<File> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn folders_and_files_are_private() {
        let d = tempfile::tempdir().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = d.path().join("a/b");
        create_dir_all(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        create_dir_all(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        create_file(&dir.join("f")).unwrap();
        assert_eq!(mode(&dir.join("f")), 0o600);
    }
}
