//! Which drive a folder lives on, found again later even if its letter or
//! mount point changed.

use crate::model::CaseMode;
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
#[cfg(windows)]
pub use windows::free_space;
#[cfg(windows)]
pub(crate) use windows::hide;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::free_space;

/// Every other system (macOS, the BSDs): builds, and reports "unsupported"
/// when asked about a drive instead of failing to compile.
#[cfg(not(any(target_os = "linux", windows)))]
mod other;
#[cfg(not(any(target_os = "linux", windows)))]
pub use other::UnsupportedVolumes as SystemVolumes;
#[cfg(not(any(unix, windows)))]
pub use other::free_space;

/// Marks a folder hidden where the system has such a flag (Windows); elsewhere
/// a dot name already hides it.
#[cfg(not(windows))]
pub(crate) fn hide(_: &std::path::Path) {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeInfo {
    /// `win:<64-bit serial>`, `win32:<32-bit serial>` or `uuid:<fs uuid>`: stored on the disk itself.
    pub id: String,
    /// Drive letter root (`E:\`) or mount point.
    pub mount_root: PathBuf,
    pub label: String,
}

pub trait Volumes {
    fn volume_of(&self, path: &Path) -> io::Result<VolumeInfo>;
    fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>>;
}

/// Whether a saved drive id names the drive that now reports `current`.
///
/// Windows has two derivations: `win:<016x>` from `FILE_ID_INFO` and
/// `win32:<08x>` from `GetVolumeInformationW`, used when a filesystem refuses
/// `FileIdInfo` (some FAT/exFAT drivers). Before 0.4.0 the second was also saved
/// as `win:`, zero-padded to 16 digits, so a `win32:` id and the `win:` id
/// holding the same 32-bit serial are treated as the same drive, both ways.
pub fn id_matches(saved: &str, current: &str) -> bool {
    saved == current
        || legacy_win_form(saved).is_some_and(|old| old == current)
        || legacy_win_form(current).is_some_and(|old| old == saved)
}

/// `win32:<8 hex digits>` as it was saved before 0.4.0: `win:` plus 16 digits.
fn legacy_win_form(id: &str) -> Option<String> {
    let serial = id.strip_prefix("win32:")?;
    (serial.len() == 8 && serial.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("win:00000000{serial}"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_match_exactly() {
        assert!(id_matches("win:0123456789abcdef", "win:0123456789abcdef"));
        assert!(id_matches("win32:89abcdef", "win32:89abcdef"));
        assert!(id_matches("uuid:1111-AAAA", "uuid:1111-AAAA"));
        assert!(!id_matches("win:0123456789abcdef", "win:0123456789abcdee"));
        assert!(!id_matches("uuid:1111-AAAA", "uuid:1111-aaaa"));
    }

    #[test]
    fn a_pre_0_4_volume_serial_id_matches_the_win32_form_both_ways() {
        // Saved before the change, read back with the fallback now prefixed `win32:`.
        assert!(id_matches("win:0000000089abcdef", "win32:89abcdef"));
        // Saved with the new prefix, the drive now answering through the old form.
        assert!(id_matches("win32:89abcdef", "win:0000000089abcdef"));
    }

    #[test]
    fn a_win32_id_never_matches_a_different_serial_or_a_full_64_bit_one() {
        assert!(!id_matches("win:0000000089abcdee", "win32:89abcdef"));
        assert!(!id_matches("win:0000000189abcdef", "win32:89abcdef"));
        assert!(!id_matches("win:89abcdef00000000", "win32:89abcdef"));
        assert!(!id_matches("win32:89abcdef", "win32:89abcdee"));
        // Malformed `win32:` ids fall back to exact comparison only.
        assert!(!id_matches("win:000000000089abcd", "win32:89abcd"));
        assert!(!id_matches("win:00000000zzzzzzzz", "win32:zzzzzzzz"));
        assert!(!id_matches("uuid:89abcdef", "win32:89abcdef"));
    }

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

    // Not Windows: there `case_mode` answers without probing, so nothing fails.
    #[cfg(unix)]
    #[test]
    fn case_mode_fails_when_the_replica_folder_is_missing() {
        let d = tempfile::tempdir().unwrap();
        let err = case_mode(&d.path().join("nope")).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn case_probe_refuses_a_folder_at_the_probe_name_and_leaves_it() {
        let d = tempfile::tempdir().unwrap();
        let planted = d.path().join(CASE_PROBE_NAME);
        std::fs::create_dir(&planted).unwrap();
        std::fs::write(planted.join("keep"), b"x").unwrap();
        let err = case_mode(d.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(planted.join("keep").exists());
    }

    #[test]
    fn free_space_is_reported() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_space(d.path()).unwrap() > 0);
    }
}
