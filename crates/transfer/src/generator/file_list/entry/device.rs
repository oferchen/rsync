//! Device number extraction for `FileEntry` rdev fields.
//!
//! Splits a raw `rdev` value into major/minor components using the
//! platform-specific bit layout (Linux split encoding vs BSD/macOS).

#[cfg(unix)]
use std::io::{Seek, SeekFrom};
#[cfg(unix)]
use std::path::Path;

/// Returns the readable byte length of a device for `--copy-devices` streaming.
///
/// A block/char device usually reports `st_size == 0`, so upstream opens the
/// device and seeks to the end to learn how many bytes the sender must stream.
/// When the stat already carries a non-zero size we trust it and skip the open.
/// A seek failure (typical for unseekable char devices) yields `0`, matching
/// upstream's `get_device_size()` fallback.
///
/// # Upstream Reference
///
/// - `flist.c:1419-1424` - `if (st.st_size == 0) { fd = do_open_checklinks(fname);
///   st.st_size = get_device_size(fd, fname); }`
/// - `flist.c:1550-1562` `get_device_size()` - `lseek(fd, 0, SEEK_END)`, returning
///   `0` on failure.
#[cfg(unix)]
pub(in crate::generator) fn device_readable_size(path: &Path, stat_size: u64) -> u64 {
    if stat_size != 0 {
        return stat_size;
    }
    match std::fs::File::open(path) {
        Ok(mut f) => f.seek(SeekFrom::End(0)).unwrap_or(0),
        Err(_) => 0,
    }
}

/// Extracts major and minor device numbers from a raw `rdev` value.
///
/// The layout differs by platform:
/// - **Linux**: Split encoding where major/minor span non-contiguous bits.
/// - **macOS/BSD**: Major in high byte, minor in low 24 bits.
///
/// # Upstream Reference
///
/// Mirrors glibc `major()`/`minor()` macros used by upstream rsync to populate
/// `rdev_major`/`rdev_minor` in `file_struct`.
#[cfg(all(unix, target_os = "linux"))]
pub(in crate::generator) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32);
    let minor = (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32);
    (major, minor)
}

/// Extracts major and minor device numbers from a raw `rdev` value (BSD/macOS).
///
/// BSD layout: major in bits 31-24, minor in bits 23-0.
#[cfg(all(unix, not(target_os = "linux")))]
pub(in crate::generator) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = (rdev >> 24) as u32;
    let minor = (rdev & 0xffffff) as u32;
    (major, minor)
}
