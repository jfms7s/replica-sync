//! Windows drive identity: the filesystem serial number stored on the disk.

use super::{VolumeInfo, Volumes};
use std::ffi::c_void;
use std::io;
use std::mem;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx, GetLogicalDrives,
    GetVolumeInformationW, GetVolumePathNameW, OPEN_EXISTING,
};
use windows_sys::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetThreadErrorMode};

#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsVolumes;

fn wide(p: &Path) -> Vec<u16> {
    p.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

fn mount_root(path: &Path) -> io::Result<PathBuf> {
    let name = wide(path);
    let mut buf = vec![0u16; 1024];
    // SAFETY: `name` is NUL-terminated; `buf` is writable for the length passed.
    let ok = unsafe { GetVolumePathNameW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(from_wide(&buf)))
}

fn serial_from_file_id(root: &Path) -> io::Result<u64> {
    let name = wide(root);
    // SAFETY: `name` is NUL-terminated; null security attributes and template are allowed.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: FILE_ID_INFO is plain data; all-zero is a valid value.
    let mut info: FILE_ID_INFO = unsafe { mem::zeroed() };
    // SAFETY: `handle` is open; `info` is writable for the size passed.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut info as *mut FILE_ID_INFO as *mut c_void,
            mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    let err = io::Error::last_os_error();
    // SAFETY: `handle` came from CreateFileW and is closed once.
    unsafe { CloseHandle(handle) };
    if ok == 0 {
        Err(err)
    } else {
        Ok(info.VolumeSerialNumber)
    }
}

fn serial_from_volume_info(root: &Path) -> io::Result<u64> {
    let name = wide(root);
    let mut serial: u32 = 0;
    // SAFETY: `name` is NUL-terminated; `serial` is a valid out-pointer; other out-pointers are null.
    let ok = unsafe {
        GetVolumeInformationW(
            name.as_ptr(),
            ptr::null_mut(),
            0,
            &mut serial,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(u64::from(serial))
    }
}

/// FILE_ID_INFO's 64-bit serial, or the 32-bit volume serial when a filesystem
/// doesn't support FileIdInfo (seen on some FAT/exFAT drivers).
fn serial_of(root: &Path) -> io::Result<u64> {
    serial_from_file_id(root).or_else(|_| serial_from_volume_info(root))
}

/// Which source the id came from, printed by `replica-sync-cli volume` for the manual exFAT check.
pub fn id_method(path: &Path) -> io::Result<&'static str> {
    let root = mount_root(&dunce::canonicalize(path)?)?;
    Ok(if serial_from_file_id(&root).is_ok() {
        "file-id-info"
    } else {
        serial_from_volume_info(&root).map(|_| "volume-serial")?
    })
}

fn label_of(root: &Path) -> String {
    let name = wide(root);
    let mut buf = [0u16; 261];
    // SAFETY: `name` is NUL-terminated; `buf` is writable; unused out-pointers are null.
    let ok = unsafe {
        GetVolumeInformationW(
            name.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            0,
        )
    };
    let label = if ok != 0 {
        from_wide(&buf)
    } else {
        String::new()
    };
    if label.is_empty() {
        root.display().to_string()
    } else {
        label
    }
}

fn id_for(serial: u64) -> String {
    format!("win:{serial:016x}")
}

impl Volumes for WindowsVolumes {
    fn volume_of(&self, path: &Path) -> io::Result<VolumeInfo> {
        let root = mount_root(&dunce::canonicalize(path)?)?;
        Ok(VolumeInfo {
            id: id_for(serial_of(&root)?),
            label: label_of(&root),
            mount_root: root,
        })
    }

    fn find(&self, id: &str) -> io::Result<Option<VolumeInfo>> {
        // SAFETY: only changes this thread's error mode (no "insert a disk" dialogs).
        unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, ptr::null_mut()) };
        // SAFETY: no arguments.
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u8 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let root = PathBuf::from(format!("{}:\\", (b'A' + i) as char));
            if serial_of(&root).is_ok_and(|s| id_for(s) == id) {
                return Ok(Some(VolumeInfo {
                    id: id.to_owned(),
                    label: label_of(&root),
                    mount_root: root,
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_drive_round_trips_through_find() {
        let vols = WindowsVolumes;
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let v = vols.volume_of(Path::new(&windir)).unwrap();
        assert!(v.id.starts_with("win:") && v.id.len() == 20, "{}", v.id);
        let found = vols.find(&v.id).unwrap().unwrap();
        assert_eq!(found.mount_root, v.mount_root);
    }

    #[test]
    fn volume_info_serial_is_non_zero() {
        assert_ne!(serial_from_volume_info(Path::new(r"C:\")).unwrap(), 0);
    }
}
