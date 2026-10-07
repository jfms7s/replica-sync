//! Linux drive identity: /proc/self/mountinfo + /dev/disk/by-uuid.

use super::{VolumeInfo, Volumes};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct LinuxVolumes {
    pub mountinfo: PathBuf,
    pub by_uuid: PathBuf,
    pub by_label: PathBuf,
}

impl Default for LinuxVolumes {
    fn default() -> Self {
        LinuxVolumes {
            mountinfo: "/proc/self/mountinfo".into(),
            by_uuid: "/dev/disk/by-uuid".into(),
            by_label: "/dev/disk/by-label".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mount {
    /// Root of the mount within its filesystem; "/" unless it is a bind mount.
    root: String,
    mount_point: PathBuf,
    source: String,
}

fn parse_mountinfo(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let f: Vec<&str> = left.split(' ').collect();
            let r: Vec<&str> = right.split(' ').collect();
            Some(Mount {
                root: unescape_octal(f.get(3)?),
                mount_point: PathBuf::from(unescape_octal(f.get(4)?)),
                source: unescape_octal(r.get(1)?),
            })
        })
        .collect()
}

/// mountinfo escapes space, tab, newline and backslash as `\NNN` (octal).
fn unescape_octal(s: &str) -> String {
    unescape(s, 3, 8, "")
}

/// udev link names escape bytes as `\xNN` (hex).
fn unescape_udev(s: &str) -> String {
    unescape(s, 2, 16, "x")
}

fn unescape(s: &str, digits: usize, radix: u32, marker: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        let start = i + 1 + marker.len();
        let code = (b[i] == b'\\' && s.get(i + 1..start) == Some(marker))
            .then(|| s.get(start..start + digits))
            .flatten()
            .and_then(|d| u8::from_str_radix(d, radix).ok());
        match code {
            Some(c) => {
                out.push(c);
                i = start + digits;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Name of the link in `dir` that resolves to `device`, if any.
fn link_name_for(dir: &Path, device: &Path) -> io::Result<Option<String>> {
    let Ok(device) = fs::canonicalize(device) else {
        return Ok(None);
    };
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    for e in entries {
        let e = e?;
        if fs::canonicalize(e.path()).ok().as_deref() == Some(device.as_path()) {
            return Ok(Some(e.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}

impl LinuxVolumes {
    fn mounts(&self) -> io::Result<Vec<Mount>> {
        Ok(parse_mountinfo(&fs::read_to_string(&self.mountinfo)?))
    }

    fn info(&self, m: &Mount) -> io::Result<Option<VolumeInfo>> {
        let Some(uuid) = link_name_for(&self.by_uuid, Path::new(&m.source))? else {
            return Ok(None);
        };
        let label = match link_name_for(&self.by_label, Path::new(&m.source))? {
            Some(l) => unescape_udev(&l),
            None => m
                .mount_point
                .file_name()
                .map_or_else(|| "/".into(), |n| n.to_string_lossy().into_owned()),
        };
        Ok(Some(VolumeInfo {
            id: format!("uuid:{uuid}"),
            mount_root: m.mount_point.clone(),
            label,
        }))
    }
}

impl Volumes for LinuxVolumes {
    fn volume_of(&self, path: &Path) -> io::Result<VolumeInfo> {
        let path = fs::canonicalize(path)?;
        let mounts = self.mounts()?;
        let best = mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| path.starts_with(&m.mount_point))
            .max_by_key(|(i, m)| (m.mount_point.components().count(), *i))
            .map(|(_, m)| m)
            .ok_or_else(|| io::Error::other("no mount found for this folder"))?;
        if best.root != "/" {
            return Err(io::Error::other(
                "folders inside bind mounts are not supported",
            ));
        }
        self.info(best)?.ok_or_else(|| {
            io::Error::other(format!(
                "cannot identify the drive mounted at {}",
                best.mount_point.display()
            ))
        })
    }

    fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>> {
        // Any absolute device path: real systems use /dev/..., the test fixture a temp folder.
        for m in self
            .mounts()?
            .iter()
            .filter(|m| m.root == "/" && m.source.starts_with('/'))
        {
            if let Some(info) = self.info(m)?
                && info.id == id
            {
                return Ok(Some(info));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    struct Fake {
        _dir: tempfile::TempDir,
        vols: LinuxVolumes,
        disk: PathBuf,
    }

    /// /, plus a USB disk mounted at <tmp>/media/My Disk (device <tmp>/dev/sdb1).
    fn fake(extra_mountinfo: &str) -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let t = dir.path();
        for d in ["dev", "by-uuid", "by-label", "media/My Disk/Photos"] {
            fs::create_dir_all(t.join(d)).unwrap();
        }
        fs::write(t.join("dev/sda2"), "").unwrap();
        fs::write(t.join("dev/sdb1"), "").unwrap();
        symlink(t.join("dev/sda2"), t.join("by-uuid/aaaa-root")).unwrap();
        symlink(t.join("dev/sdb1"), t.join("by-uuid/1111-AAAA")).unwrap();
        symlink(t.join("dev/sdb1"), t.join("by-label/My\\x20Disk")).unwrap();
        let media = t.join("media").to_string_lossy().into_owned();
        let mountinfo = format!(
            "22 1 8:2 / / rw,relatime shared:1 - ext4 {dev}/sda2 rw\n\
             90 22 8:17 / {media}/My\\040Disk rw,nosuid shared:50 - exfat {dev}/sdb1 rw\n{extra_mountinfo}",
            dev = t.join("dev").display(),
        );
        fs::write(t.join("mountinfo"), mountinfo).unwrap();
        let vols = LinuxVolumes {
            mountinfo: t.join("mountinfo"),
            by_uuid: t.join("by-uuid"),
            by_label: t.join("by-label"),
        };
        let disk = fs::canonicalize(t.join("media/My Disk")).unwrap();
        Fake {
            _dir: dir,
            vols,
            disk,
        }
    }

    #[test]
    fn parses_mountinfo_with_escaped_spaces() {
        let m = parse_mountinfo("90 22 8:17 / /media/My\\040Disk rw - exfat /dev/sdb1 rw\n");
        assert_eq!(
            m,
            vec![Mount {
                root: "/".into(),
                mount_point: "/media/My Disk".into(),
                source: "/dev/sdb1".into()
            }]
        );
    }

    #[test]
    fn volume_of_picks_longest_mount_and_reads_uuid_and_label() {
        let f = fake("");
        let v = f.vols.volume_of(&f.disk.join("Photos")).unwrap();
        assert_eq!(v.id, "uuid:1111-AAAA");
        assert_eq!(v.label, "My Disk");
        assert_eq!(v.mount_root, f.disk);
    }

    #[test]
    fn find_returns_the_mount_root_for_an_id() {
        let f = fake("");
        assert_eq!(
            f.vols.find("uuid:1111-AAAA").unwrap().unwrap().mount_root,
            f.disk
        );
        assert!(f.vols.find("uuid:nope").unwrap().is_none());
    }

    #[test]
    fn bind_mounts_are_refused() {
        let f = fake("");
        let bind = format!(
            "{}\n91 22 8:17 /sub {} rw - exfat /dev/sdb1 rw\n",
            fs::read_to_string(&f.vols.mountinfo).unwrap().trim_end(),
            f.disk
                .join("Photos")
                .display()
                .to_string()
                .replace(' ', "\\040")
        );
        fs::write(&f.vols.mountinfo, bind).unwrap();
        assert!(f.vols.volume_of(&f.disk.join("Photos")).is_err());
    }
}
