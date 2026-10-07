//! Placeholder until Task 13 implements Windows drive identity.

use super::{VolumeInfo, Volumes};
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsVolumes;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Windows drive identity is not implemented yet",
    ))
}

impl Volumes for WindowsVolumes {
    fn volume_of(&self, _path: &Path) -> io::Result<VolumeInfo> {
        unsupported()
    }

    fn find(&self, _id: &str) -> io::Result<Option<VolumeInfo>> {
        unsupported()
    }
}
