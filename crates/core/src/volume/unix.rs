//! What every Unix answers the same way.

use std::io;
use std::path::Path;

/// Bytes free for an unprivileged user on the filesystem holding `path`.
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
