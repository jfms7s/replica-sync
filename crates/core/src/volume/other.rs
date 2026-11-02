//! Systems without drive identity support yet (macOS, the BSDs): every
//! lookup reports `Unsupported`, so the app says so instead of not building.

use super::{VolumeInfo, Volumes};
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct UnsupportedVolumes;

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "drive identity is not supported on this system",
    )
}

impl Volumes for UnsupportedVolumes {
    fn volume_of(&self, _: &Path) -> io::Result<VolumeInfo> {
        Err(unsupported())
    }

    fn find(&self, _: &str) -> io::Result<Option<VolumeInfo>> {
        Err(unsupported())
    }
}

#[cfg(not(any(unix, windows)))]
pub fn free_space(_: &Path) -> io::Result<u64> {
    Err(unsupported())
}
