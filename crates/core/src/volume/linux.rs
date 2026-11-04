//! Linux drive identity: /proc/self/mountinfo + /dev/disk/by-uuid.
//!
//! A mount whose root within its filesystem is not `/` (a btrfs subvolume or a
//! bind mount) gets the id `uuid:<uuid>:<root>`, so it is told apart from the
//! whole filesystem.

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
        let id = if m.root == "/" {
            format!("uuid:{uuid}")
        } else {
            format!("uuid:{uuid}:{}", m.root)
        };
        Ok(Some(VolumeInfo {
            id,
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
        self.info(best)?.ok_or_else(|| {
            io::Error::other(format!(
                "cannot identify the drive mounted at {}",
                best.mount_point.display()
            ))
        })
    }

    fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>> {
        // Any absolute device path: real systems use /dev/..., the test fixture a temp folder.
        for m in self.mounts()?.iter().filter(|m| m.source.starts_with('/')) {
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
    fn subvolume_or_bind_mounts_get_a_rooted_id() {
        let f = fake("");
        let dev = f._dir.path().join("dev");
        let bind = format!(
            "{}\n91 22 8:17 /sub {} rw - exfat {dev}/sdb1 rw\n",
            fs::read_to_string(&f.vols.mountinfo).unwrap().trim_end(),
            f.disk
                .join("Photos")
                .display()
                .to_string()
                .replace(' ', "\\040"),
            dev = dev.display()
        );
        fs::write(&f.vols.mountinfo, bind).unwrap();
        let photos = f.disk.join("Photos");
        let v = f.vols.volume_of(&photos).unwrap();
        assert_eq!(v.id, "uuid:1111-AAAA:/sub");
        assert_eq!(v.mount_root, photos);
        assert_eq!(
            f.vols
                .find("uuid:1111-AAAA:/sub")
                .unwrap()
                .unwrap()
                .mount_root,
            photos
        );
        assert_eq!(
            f.vols.find("uuid:1111-AAAA").unwrap().unwrap().mount_root,
            f.disk
        );
    }

    /// A second device mounted on the same point as the USB disk (an
    /// over-mount): `<tmp>/dev/sdc1`, uuid `2222-BBBB`.
    fn second_device(f: &Fake) -> String {
        let t = f._dir.path();
        fs::write(t.join("dev/sdc1"), "").unwrap();
        symlink(t.join("dev/sdc1"), t.join("by-uuid/2222-BBBB")).unwrap();
        format!(
            "92 22 8:33 / {}/media/My\\040Disk rw - ext4 {}/sdc1 rw",
            t.display(),
            t.join("dev").display()
        )
    }

    #[test]
    fn the_later_of_two_mounts_on_one_point_wins() {
        let f = fake("");
        let over = second_device(&f);
        let original = fs::read_to_string(&f.vols.mountinfo).unwrap();
        let photos = f.disk.join("Photos");

        fs::write(&f.vols.mountinfo, format!("{original}{over}\n")).unwrap();
        assert_eq!(f.vols.volume_of(&photos).unwrap().id, "uuid:2222-BBBB");
        assert_eq!(
            f.vols.find("uuid:2222-BBBB").unwrap().unwrap().mount_root,
            f.disk
        );

        // The order in the file decides, not the device or the id.
        let lines: Vec<&str> = original.lines().collect();
        fs::write(
            &f.vols.mountinfo,
            format!("{}\n{over}\n{}\n", lines[0], lines[1]),
        )
        .unwrap();
        assert_eq!(f.vols.volume_of(&photos).unwrap().id, "uuid:1111-AAAA");
    }

    #[test]
    fn without_a_label_link_the_label_is_the_mount_point_name() {
        let f = fake("");
        fs::remove_file(f._dir.path().join("by-label/My\\x20Disk")).unwrap();
        let v = f.vols.volume_of(&f.disk).unwrap();
        assert_eq!(v.id, "uuid:1111-AAAA");
        assert_eq!(v.label, "My Disk");
        // The root mount has no name to fall back on.
        let v = f.vols.volume_of(f._dir.path()).unwrap();
        assert_eq!(v.id, "uuid:aaaa-root");
        assert_eq!(v.label, "/");
    }

    #[test]
    fn a_mount_without_a_uuid_link_cannot_be_identified() {
        let f = fake("");
        fs::remove_file(f._dir.path().join("by-uuid/1111-AAAA")).unwrap();
        let err = f.vols.volume_of(&f.disk).unwrap_err();
        assert!(err.to_string().contains("cannot identify"), "{err}");
        assert!(f.vols.find("uuid:1111-AAAA").unwrap().is_none());
    }

    #[test]
    fn unescape_udev_decodes_hex_escapes_and_leaves_bad_ones_alone() {
        assert_eq!(unescape_udev("My\\x20Disk"), "My Disk");
        assert_eq!(unescape_udev("\\x41\\x42c"), "ABc");
        assert_eq!(unescape_udev("caf\\xc3\\xa9"), "caf\u{e9}");
        assert_eq!(unescape_udev("plain"), "plain");
        assert_eq!(unescape_udev(""), "");
        // Not valid hex, too short, a lone backslash, octal instead of hex.
        assert_eq!(unescape_udev("a\\xZZb"), "a\\xZZb");
        assert_eq!(unescape_udev("a\\x2"), "a\\x2");
        assert_eq!(unescape_udev("a\\"), "a\\");
        assert_eq!(unescape_udev("a\\040b"), "a\\040b");
        // Bytes that are not UTF-8 become the replacement character.
        assert_eq!(unescape_udev("\\xff"), "\u{fffd}");
    }
}
