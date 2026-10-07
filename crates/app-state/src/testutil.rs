//! Fake drives for session tests: each "drive" is a temp folder with a fixed id.

#![allow(dead_code)]

use replica_sync_core::volume::{VolumeInfo, Volumes};
use std::cell::RefCell;
use std::fs;
use std::io;
use std::path::Path;

#[derive(Default)]
pub struct FakeVolumes(pub RefCell<Vec<VolumeInfo>>);

impl FakeVolumes {
    pub fn mount(&self, id: &str, root: &Path) {
        let root = dunce::canonicalize(root).unwrap();
        self.0.borrow_mut().retain(|v| v.id != id);
        self.0.borrow_mut().push(VolumeInfo {
            id: id.into(),
            mount_root: root,
            label: format!("Disk {id}"),
        });
    }
    pub fn unmount(&self, id: &str) {
        self.0.borrow_mut().retain(|v| v.id != id);
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
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown drive"))
    }
    fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>> {
        Ok(self.0.borrow().iter().find(|v| v.id == id).cloned())
    }
}

pub fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    filetime::set_file_mtime(&p, filetime::FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
}
