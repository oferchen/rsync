//! Portable readable-size probing for `--copy-devices`.
//!
//! Mirrors upstream rsync's `flist.c:get_device_size()`, which is an
//! `lseek(fd, 0, SEEK_END)`. That returns the block-device byte length on Linux
//! but `0` on macOS/BSD, where raw block devices are not seekable. On Apple
//! targets we fall back to the `DKIOCGETBLOCKSIZE`/`DKIOCGETBLOCKCOUNT` ioctls
//! (`<sys/disk.h>`) to recover the real size, so `--copy-devices` streams the
//! full device contents there too.
//!
//! Character devices (and any device that reports neither a seek length nor a
//! block count) yield `0`, matching upstream: their length is unknown/infinite
//! and rsync streams zero bytes rather than reading forever.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::path::Path;

/// Returns the readable byte length of the block/char device at `path`.
///
/// Opens the device read-only and probes its size via `lseek(SEEK_END)`, then
/// (on Apple targets, where that yields `0`) via the disk ioctls. Returns `0`
/// when the size cannot be determined - the upstream behaviour for unseekable
/// character devices.
///
/// # Upstream Reference
///
/// - `flist.c:1419-1424` `make_file()` - opens the device and calls
///   `get_device_size()` when a `--copy-devices` entry reports `st_size == 0`.
/// - `flist.c:1550-1562` `get_device_size()` - `lseek(fd, 0, SEEK_END)`.
#[cfg(unix)]
pub fn device_readable_size(path: &Path) -> io::Result<u64> {
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    // upstream: flist.c:1552 - lseek(fd, 0, SEEK_END). Authoritative on Linux.
    let via_seek = file.seek(SeekFrom::End(0)).unwrap_or(0);
    if via_seek > 0 {
        return Ok(via_seek);
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    if let Some(size) = apple_block_device_size(&file) {
        return Ok(size);
    }

    Ok(via_seek)
}

/// Recovers a block device's byte length on Apple targets via the disk ioctls.
///
/// macOS block devices return `0` from `lseek(SEEK_END)`, so the size must come
/// from `DKIOCGETBLOCKSIZE` * `DKIOCGETBLOCKCOUNT`. Returns `None` when the fd
/// is not a block device or the ioctls fail (e.g. a character device).
#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)] // REASON: disk-size ioctls have no safe libc wrapper.
fn apple_block_device_size(file: &std::fs::File) -> Option<u64> {
    use std::os::unix::io::AsRawFd;

    // <sys/disk.h>: _IOR('d', 24, uint32_t) and _IOR('d', 25, uint64_t).
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

    let fd = file.as_raw_fd();
    let mut block_size: u32 = 0;
    let mut block_count: u64 = 0;

    // SAFETY: `fd` is a live descriptor owned by `file`. Each ioctl writes
    // exactly the fixed-width integer its `_IOR` encoding declares (a `u32` and
    // a `u64` respectively) into the provided stack slot; a non-zero return
    // leaves the slot untouched and is treated as "unknown".
    unsafe {
        if libc::ioctl(fd, DKIOCGETBLOCKSIZE, &mut block_size) != 0 {
            return None;
        }
        if libc::ioctl(fd, DKIOCGETBLOCKCOUNT, &mut block_count) != 0 {
            return None;
        }
    }

    Some(block_count.saturating_mul(u64::from(block_size)))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A character device with no seekable length yields `0`, matching
    /// upstream's `get_device_size()` for unseekable devices.
    #[test]
    fn char_device_reports_zero() {
        let dev = Path::new("/dev/zero");
        let Ok(meta) = std::fs::symlink_metadata(dev) else {
            eprintln!("skipping: /dev/zero unavailable");
            return;
        };
        use std::os::unix::fs::FileTypeExt;
        if !meta.file_type().is_char_device() {
            eprintln!("skipping: /dev/zero is not a char device here");
            return;
        }
        assert_eq!(device_readable_size(dev).unwrap(), 0);
    }
}
