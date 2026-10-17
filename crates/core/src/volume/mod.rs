//! Which drive a folder lives on, found again later even if its letter or
//! mount point changed.

use crate::diff::CaseMode;
use crate::rules::CASE_PROBE_NAME;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxVolumes as SystemVolumes;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::WindowsVolumes as SystemVolumes;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// `win:<serial>` or `uuid:<fs uuid>`: stored on the disk itself.
    pub id: String,
    /// Drive letter root (`E:\`) or mount point.
    pub mount_root: PathBuf,
    pub label: String,
}

pub trait Volumes {
    fn volume_of(&self, path: &Path) -> io::Result<VolumeInfo>;
    fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>>;
}

#[allow(clippy::default_constructed_unit_structs)] // a unit struct on Windows, not on Linux
pub fn system() -> SystemVolumes {
    SystemVolumes::default()
}

/// Whether names on the drive holding `dir` ignore capital letters.
/// On Linux this writes and removes a probe file in `dir` (always the replica).
pub fn case_mode(dir: &Path) -> io::Result<CaseMode> {
    if cfg!(windows) {
        return Ok(CaseMode::Insensitive);
    }
    let probe = dir.join(CASE_PROBE_NAME);
    create_probe(&probe)?;
    let upper = fs::symlink_metadata(dir.join(CASE_PROBE_NAME.to_uppercase()));
    fs::remove_file(&probe)?;
    let insensitive = match upper {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(e),
    };
    Ok(if insensitive {
        CaseMode::Insensitive
    } else {
        CaseMode::Sensitive
    })
}

/// Creates the probe file without following anything already at its name:
/// `create_new` fails on an existing entry, symlinks included. A regular file
/// left there by a crash is removed and the create retried once; anything
/// else (a planted link, a folder) is refused.
fn create_probe(probe: &Path) -> io::Result<()> {
    let create = || OpenOptions::new().write(true).create_new(true).open(probe);
    match create() {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::symlink_metadata(probe)?.file_type().is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "the backup drive contains an unexpected link at {}",
                        probe.display()
                    ),
                ));
            }
            fs::remove_file(probe)?;
            create().map(|_| ())
        }
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // statvfs field widths differ between platforms
pub fn free_space(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `c` is NUL-terminated and `st` is a valid out-pointer.
    if unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: statvfs returned 0, so it filled `st`.
    let st = unsafe { st.assume_init() };
    Ok(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(windows)]
pub fn free_space(path: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut free = 0u64;
    // SAFETY: `name` is NUL-terminated; `free` is a valid out-pointer; the others may be null.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            name.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(free)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_probe_on_tmp_is_sensitive_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            case_mode(d.path()).unwrap(),
            if cfg!(windows) {
                CaseMode::Insensitive
            } else {
                CaseMode::Sensitive
            }
        );
        assert!(std::fs::read_dir(d.path()).unwrap().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn case_probe_refuses_planted_symlink() {
        let base = tempfile::tempdir().unwrap();
        let replica = base.path().join("replica");
        let outside = base.path().join("outside");
        std::fs::create_dir_all(&replica).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("victim.txt");
        std::fs::write(&victim, b"keep me").unwrap();
        let link = replica.join(CASE_PROBE_NAME);
        std::os::unix::fs::symlink("../outside/victim.txt", &link).unwrap();

        let err = case_mode(&replica).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[cfg(unix)]
    #[test]
    fn case_probe_replaces_leftover_probe_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(CASE_PROBE_NAME), b"stale").unwrap();
        assert_eq!(case_mode(d.path()).unwrap(), CaseMode::Sensitive);
        assert!(std::fs::read_dir(d.path()).unwrap().next().is_none());
    }

    #[test]
    fn free_space_is_reported() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space(d.path()).unwrap() > 0);
    }
}
