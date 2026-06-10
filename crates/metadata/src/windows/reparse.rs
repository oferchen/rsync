#![cfg(target_os = "windows")]
#![allow(unsafe_code)]

//! Windows NTFS reparse-point classification.
//!
//! Uses the `windows` crate FFI surface to read the leading
//! `REPARSE_DATA_BUFFER` header via `DeviceIoControl(FSCTL_GET_REPARSE_POINT)`
//! and classifies the entry by its `ReparseTag`.
//!
//! Upstream rsync has no native reparse-point handling; on Windows it runs
//! under Cygwin, which treats every reparse point as a POSIX symbolic link.
//! Native `oc-rsync` distinguishes the four common NTFS shapes:
//!
//! - `mklink` symlinks (`IO_REPARSE_TAG_SYMLINK`)
//! - `mklink /j` directory junctions and volume mount-points
//!   (`IO_REPARSE_TAG_MOUNT_POINT`, disambiguated by the substitute-name
//!   prefix - mount-points use `\??\Volume{GUID}\`)
//! - OneDrive / cloud placeholders (`IO_REPARSE_TAG_CLOUD*`,
//!   `IO_REPARSE_TAG_ONEDRIVE`)
//! - WSL `AF_UNIX` sockets (`IO_REPARSE_TAG_AF_UNIX`)
//!
//! Tag constants follow `winnt.h`. Unknown tags surface as
//! [`ReparseKind::Other`] so callers can choose to skip, error, or fall
//! through. Deeper payload parsing (substitute names, print names, symlink
//! flags) is intentionally deferred to follow-up tasks.

use std::io;
use std::os::windows::io::AsRawHandle;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;

/// Maximum size of a reparse data buffer, per `winnt.h`.
///
/// Microsoft fixes this at 16 KiB. The `windows` crate exposes the constant
/// under several modules across versions; we pin the value locally to avoid
/// churn and to keep the type explicit.
const MAXIMUM_REPARSE_DATA_BUFFER_SIZE: usize = 16 * 1024;

/// `IO_REPARSE_TAG_SYMLINK` from `winnt.h`.
const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
/// `IO_REPARSE_TAG_MOUNT_POINT` from `winnt.h` (covers junctions and
/// volume mount-points; the kind is disambiguated by the substitute-name
/// prefix).
const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `IO_REPARSE_TAG_AF_UNIX` (WSL `AF_UNIX` socket file).
const IO_REPARSE_TAG_AF_UNIX: u32 = 0x8000_0023;
/// `IO_REPARSE_TAG_CLOUD` legacy cloud-files placeholder tag.
const IO_REPARSE_TAG_CLOUD: u32 = 0x9000_001A;
/// `IO_REPARSE_TAG_ONEDRIVE` legacy OneDrive placeholder tag.
const IO_REPARSE_TAG_ONEDRIVE: u32 = 0x9000_001B;

/// Mask used to detect modern cloud-files reparse tags
/// (`IO_REPARSE_TAG_CLOUD_*`, range `0x9000_0010..=0x9000_001F`).
///
/// Modern Windows builds allocate a contiguous block of tag values for the
/// cloud-files API, and the `windows` crate exposes them individually; we
/// match the range rather than enumerating every constant.
const CLOUD_TAG_RANGE_START: u32 = 0x9000_0010;
/// Upper bound (inclusive) of the cloud-files tag range.
const CLOUD_TAG_RANGE_END: u32 = 0x9000_001F;

/// Byte offset of `ReparseTag` inside `REPARSE_DATA_BUFFER`.
const REPARSE_TAG_OFFSET: usize = 0;
/// Byte offset of `ReparseDataLength` inside `REPARSE_DATA_BUFFER`.
const REPARSE_DATA_LENGTH_OFFSET: usize = 4;
/// Byte offset of the payload (after `ReparseTag` + `ReparseDataLength` +
/// `Reserved`).
const REPARSE_HEADER_SIZE: usize = 8;

/// `SubstituteNameOffset` field offset inside the `MountPointReparseBuffer`
/// payload (immediately following the common header).
const MOUNT_POINT_SUBSTITUTE_NAME_OFFSET: usize = REPARSE_HEADER_SIZE;
/// `SubstituteNameLength` field offset inside `MountPointReparseBuffer`.
const MOUNT_POINT_SUBSTITUTE_NAME_LENGTH: usize = REPARSE_HEADER_SIZE + 2;
/// Offset of the variable-length `PathBuffer` inside
/// `MountPointReparseBuffer` (after the four `u16` offset/length fields).
const MOUNT_POINT_PATH_BUFFER_OFFSET: usize = REPARSE_HEADER_SIZE + 8;

/// Classification of an NTFS reparse-point entry.
///
/// Variants correspond to the well-known `IO_REPARSE_TAG_*` values from
/// `winnt.h`. The `MountPoint` and `Junction` variants share the same
/// `IO_REPARSE_TAG_MOUNT_POINT` tag and are distinguished by the substitute-
/// name prefix: volume mount-points use `\??\Volume{GUID}\`, while
/// `mklink /j` junctions point at directory paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparseKind {
    /// Symbolic link created by `mklink` (file or directory).
    /// Tag: `IO_REPARSE_TAG_SYMLINK` (`0xA000000C`).
    Symlink,
    /// Directory junction created by `mklink /j`.
    /// Tag: `IO_REPARSE_TAG_MOUNT_POINT` (`0xA0000003`) with a non-volume
    /// substitute-name prefix.
    Junction,
    /// Volume mount-point exposing a separate volume at a directory path.
    /// Tag: `IO_REPARSE_TAG_MOUNT_POINT` (`0xA0000003`) with a
    /// `\??\Volume{GUID}\` substitute-name prefix.
    MountPoint,
    /// OneDrive or generic cloud-files placeholder.
    /// Tag: `IO_REPARSE_TAG_ONEDRIVE` (`0x9000001B`),
    /// `IO_REPARSE_TAG_CLOUD` (`0x9000001A`), or any tag in the
    /// `IO_REPARSE_TAG_CLOUD_*` range (`0x90000010..=0x9000001F`).
    OneDrive,
    /// WSL `AF_UNIX` socket file. Tag: `IO_REPARSE_TAG_AF_UNIX`
    /// (`0x80000023`).
    AfUnix,
    /// Unknown or unsupported reparse tag. Carries the raw `ReparseTag`
    /// value so callers can log it, skip the entry, or decide to error.
    Other(u32),
}

/// Classify a reparse-point by reading `FSCTL_GET_REPARSE_POINT` from a
/// handle opened with `FILE_FLAG_OPEN_REPARSE_POINT`.
///
/// # Preconditions
///
/// `handle` must refer to an open Win32 file handle obtained with
/// `CreateFileW`/`File::open` using `FILE_FLAG_OPEN_REPARSE_POINT` so the
/// reparse data is returned rather than followed. The handle must remain
/// valid for the duration of this call.
///
/// # Errors
///
/// Returns the last OS error if `DeviceIoControl` fails (for example, if
/// the file is not a reparse point or the handle was opened without
/// reparse-point access). The error preserves the raw Win32 error code so
/// callers can surface it through [`crate::MetadataError`].
///
/// # Behaviour
///
/// On success the leading `REPARSE_DATA_BUFFER` header is inspected and
/// the entry is classified via [`classify_from_buffer`]. Unknown tags
/// surface as [`ReparseKind::Other`].
pub fn classify_reparse_point(handle: &impl AsRawHandle) -> io::Result<ReparseKind> {
    let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
    let mut bytes_returned: u32 = 0;
    let raw = handle.as_raw_handle();
    let h = HANDLE(raw.cast());

    // SAFETY: `h` is a valid Win32 file handle borrowed for the duration of
    // the call (the caller's `&impl AsRawHandle` lifetime outlives this
    // function). `buffer` is heap-owned and exactly
    // `MAXIMUM_REPARSE_DATA_BUFFER_SIZE` bytes long, matching the maximum
    // payload the kernel can return. `bytes_returned` is a valid stack
    // pointer. No input buffer is required for `FSCTL_GET_REPARSE_POINT`.
    let result = unsafe {
        DeviceIoControl(
            h,
            FSCTL_GET_REPARSE_POINT,
            None,
            0,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut bytes_returned),
            None,
        )
    };

    result.map_err(|_| io::Error::last_os_error())?;

    let returned = bytes_returned as usize;
    if returned < REPARSE_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "FSCTL_GET_REPARSE_POINT returned {returned} bytes, expected at least {REPARSE_HEADER_SIZE}"
            ),
        ));
    }

    Ok(classify_from_buffer(&buffer[..returned]))
}

/// Classify a reparse-point from an in-memory `REPARSE_DATA_BUFFER` byte
/// slice without touching the OS.
///
/// Exposed at `pub(crate)` for unit tests and any future caller that
/// already holds the raw buffer (for example a cached metadata blob from
/// `FindFirstFileExW` follow-ups). Reads only the four-byte `ReparseTag`
/// at offset zero, then peeks at the `MountPointReparseBuffer`
/// substitute-name prefix when needed to split
/// [`ReparseKind::MountPoint`] from [`ReparseKind::Junction`].
///
/// Buffers shorter than the 8-byte `REPARSE_DATA_BUFFER` header classify
/// as [`ReparseKind::Other`] with a tag of zero to avoid panics on
/// malformed input; callers that require strict validation should check
/// the length before calling.
pub(crate) fn classify_from_buffer(buf: &[u8]) -> ReparseKind {
    if buf.len() < REPARSE_HEADER_SIZE {
        return ReparseKind::Other(0);
    }

    let tag = read_u32_le(buf, REPARSE_TAG_OFFSET);

    match tag {
        IO_REPARSE_TAG_SYMLINK => ReparseKind::Symlink,
        IO_REPARSE_TAG_MOUNT_POINT => classify_mount_point(buf),
        IO_REPARSE_TAG_AF_UNIX => ReparseKind::AfUnix,
        IO_REPARSE_TAG_ONEDRIVE | IO_REPARSE_TAG_CLOUD => ReparseKind::OneDrive,
        t if (CLOUD_TAG_RANGE_START..=CLOUD_TAG_RANGE_END).contains(&t) => ReparseKind::OneDrive,
        t => ReparseKind::Other(t),
    }
}

/// Distinguish a volume mount-point from a directory junction by peeking
/// at the substitute-name prefix inside the `MountPointReparseBuffer`
/// payload.
///
/// Both shapes share `IO_REPARSE_TAG_MOUNT_POINT`. Volume mount-points
/// use the `\??\Volume{GUID}\` NT-namespace prefix; directory junctions
/// point at filesystem paths (typically `\??\C:\...`). When the payload
/// is too short to contain a substitute name we conservatively classify
/// as [`ReparseKind::Junction`] since that is the more common shape and
/// the one with the broader use-case in transfer pipelines.
fn classify_mount_point(buf: &[u8]) -> ReparseKind {
    let data_length = read_u16_le(buf, REPARSE_DATA_LENGTH_OFFSET) as usize;
    let payload_end = REPARSE_HEADER_SIZE + data_length;
    if buf.len() < payload_end || data_length < (MOUNT_POINT_PATH_BUFFER_OFFSET - REPARSE_HEADER_SIZE) {
        return ReparseKind::Junction;
    }

    let substitute_offset = read_u16_le(buf, MOUNT_POINT_SUBSTITUTE_NAME_OFFSET) as usize;
    let substitute_length = read_u16_le(buf, MOUNT_POINT_SUBSTITUTE_NAME_LENGTH) as usize;
    if substitute_length == 0 {
        return ReparseKind::Junction;
    }

    let path_buffer_start = MOUNT_POINT_PATH_BUFFER_OFFSET;
    let name_start = path_buffer_start + substitute_offset;
    let name_end = name_start + substitute_length;
    if name_end > buf.len() || (name_end - name_start) % size_of::<u16>() != 0 {
        return ReparseKind::Junction;
    }

    let wide: Vec<u16> = buf[name_start..name_end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();

    if substitute_name_is_volume(&wide) {
        ReparseKind::MountPoint
    } else {
        ReparseKind::Junction
    }
}

/// Returns `true` when a substitute-name UTF-16 slice starts with the
/// `\??\Volume{` NT-namespace prefix that volume mount-points use.
///
/// Case-insensitive on the literal `Volume` segment, matching Windows
/// path semantics. Directory junctions never produce this prefix.
fn substitute_name_is_volume(wide: &[u16]) -> bool {
    const PREFIX: &[u16] = &[
        b'\\' as u16,
        b'?' as u16,
        b'?' as u16,
        b'\\' as u16,
        b'V' as u16,
        b'o' as u16,
        b'l' as u16,
        b'u' as u16,
        b'm' as u16,
        b'e' as u16,
        b'{' as u16,
    ];
    if wide.len() < PREFIX.len() {
        return false;
    }
    wide.iter()
        .zip(PREFIX.iter())
        .all(|(a, b)| ascii_eq_ignore_case(*a, *b))
}

/// ASCII-case-insensitive equality for UTF-16 code units in the BMP
/// ASCII range. Non-ASCII code units compare strictly.
fn ascii_eq_ignore_case(a: u16, b: u16) -> bool {
    if a < 0x80 && b < 0x80 {
        (a as u8).eq_ignore_ascii_case(&(b as u8))
    } else {
        a == b
    }
}

/// Read a little-endian `u32` from `buf` at `offset`. Caller must ensure
/// `offset + 4 <= buf.len()`.
fn read_u32_le(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

/// Read a little-endian `u16` from `buf` at `offset`. Caller must ensure
/// `offset + 2 <= buf.len()`.
fn read_u16_le(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a UTF-16 little-endian byte sequence for a `&str`.
    fn utf16_le(s: &str) -> Vec<u8> {
        s.encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect()
    }

    /// Build a synthetic `MOUNT_POINT_REPARSE_BUFFER` with the supplied
    /// substitute-name. The print-name is left empty for brevity since
    /// the classifier does not consult it.
    fn build_mount_point_buffer(substitute_name: &str) -> Vec<u8> {
        let sub_utf16 = utf16_le(substitute_name);
        let sub_len = sub_utf16.len();
        // PathBuffer layout: [substitute_name][null-term][print_name (empty)][null-term]
        let path_buffer_len = sub_len + 2 + 2;
        let data_length = 8 + path_buffer_len; // 4 u16 fields + variable PathBuffer

        let mut buf = Vec::with_capacity(REPARSE_HEADER_SIZE + data_length);
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend_from_slice(&(data_length as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        // SubstituteNameOffset = 0 (start of PathBuffer)
        buf.extend_from_slice(&0u16.to_le_bytes());
        // SubstituteNameLength = sub_len
        buf.extend_from_slice(&(sub_len as u16).to_le_bytes());
        // PrintNameOffset = sub_len + 2 (after sub-name null-term)
        buf.extend_from_slice(&((sub_len + 2) as u16).to_le_bytes());
        // PrintNameLength = 0
        buf.extend_from_slice(&0u16.to_le_bytes());
        // PathBuffer
        buf.extend_from_slice(&sub_utf16);
        buf.extend_from_slice(&[0, 0]); // sub-name null-term
        buf.extend_from_slice(&[0, 0]); // print-name null-term

        buf
    }

    fn build_simple_tag(tag: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(REPARSE_HEADER_SIZE);
        buf.extend_from_slice(&tag.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // ReparseDataLength = 0
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf
    }

    #[test]
    fn classifies_symlink_tag() {
        let buf = build_simple_tag(IO_REPARSE_TAG_SYMLINK);
        assert_eq!(classify_from_buffer(&buf), ReparseKind::Symlink);
    }

    #[test]
    fn classifies_mount_point_volume_prefix_as_mount_point() {
        let buf =
            build_mount_point_buffer("\\??\\Volume{12345678-1234-1234-1234-1234567890ab}\\");
        assert_eq!(classify_from_buffer(&buf), ReparseKind::MountPoint);
    }

    #[test]
    fn classifies_mount_point_directory_prefix_as_junction() {
        let buf = build_mount_point_buffer("\\??\\C:\\Users\\target");
        assert_eq!(classify_from_buffer(&buf), ReparseKind::Junction);
    }

    #[test]
    fn classifies_mount_point_volume_prefix_case_insensitive() {
        let buf = build_mount_point_buffer("\\??\\VOLUME{ABCDEF12-3456-7890-ABCD-EF1234567890}\\");
        assert_eq!(classify_from_buffer(&buf), ReparseKind::MountPoint);
    }

    #[test]
    fn classifies_af_unix_tag() {
        let buf = build_simple_tag(IO_REPARSE_TAG_AF_UNIX);
        assert_eq!(classify_from_buffer(&buf), ReparseKind::AfUnix);
    }

    #[test]
    fn classifies_onedrive_legacy_tag() {
        let buf = build_simple_tag(IO_REPARSE_TAG_ONEDRIVE);
        assert_eq!(classify_from_buffer(&buf), ReparseKind::OneDrive);
    }

    #[test]
    fn classifies_cloud_legacy_tag() {
        let buf = build_simple_tag(IO_REPARSE_TAG_CLOUD);
        assert_eq!(classify_from_buffer(&buf), ReparseKind::OneDrive);
    }

    #[test]
    fn classifies_cloud_range_tags() {
        for tag in CLOUD_TAG_RANGE_START..=CLOUD_TAG_RANGE_END {
            let buf = build_simple_tag(tag);
            assert_eq!(
                classify_from_buffer(&buf),
                ReparseKind::OneDrive,
                "tag {tag:#010x} should map to OneDrive"
            );
        }
    }

    #[test]
    fn classifies_unknown_tag_as_other() {
        let tag = 0xDEAD_BEEF_u32;
        let buf = build_simple_tag(tag);
        assert_eq!(classify_from_buffer(&buf), ReparseKind::Other(tag));
    }

    #[test]
    fn rejects_truncated_buffer() {
        let buf = vec![0u8; REPARSE_HEADER_SIZE - 1];
        assert_eq!(classify_from_buffer(&buf), ReparseKind::Other(0));
    }

    #[test]
    fn mount_point_with_zero_substitute_length_is_junction() {
        // Build a mount-point buffer with the substitute-name fields zeroed out.
        let mut buf = Vec::new();
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend_from_slice(&12u16.to_le_bytes()); // ReparseDataLength
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        // SubstituteNameOffset, SubstituteNameLength, PrintNameOffset, PrintNameLength = 0
        buf.extend_from_slice(&[0u8; 8]);
        // Empty PathBuffer terminators
        buf.extend_from_slice(&[0u8; 4]);
        assert_eq!(classify_from_buffer(&buf), ReparseKind::Junction);
    }
}

#[cfg(all(test, target_os = "windows"))]
mod integration_tests {
    //! Integration tests that materialise real NTFS reparse points and
    //! invoke [`classify_reparse_point`] against an open `CreateFileW`
    //! handle.
    //!
    //! `mklink /j` (directory junction) does not require administrator
    //! privileges on Windows 10+, so the junction test runs unconditionally
    //! and provides end-to-end coverage of the
    //! `DeviceIoControl`/`FSCTL_GET_REPARSE_POINT` code path. Symlink
    //! creation requires either developer mode or admin and is skipped at
    //! runtime when the test environment lacks the privilege rather than
    //! failing the build.

    use super::*;
    use std::fs;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::process::Command;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    const FILE_READ_ATTRIBUTES: u32 = 0x0080;

    struct OwnedHandle(HANDLE);

    impl std::os::windows::io::AsRawHandle for OwnedHandle {
        fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
            self.0.0 as std::os::windows::io::RawHandle
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` was returned by `CreateFileW` above and has
            // not been closed elsewhere. Ownership is unique because the
            // handle lives only inside this guard.
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.0);
            }
        }
    }

    fn open_reparse(path: &Path) -> io::Result<OwnedHandle> {
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `wide` is a null-terminated UTF-16 path slice owned for
        // the duration of the call. `FILE_FLAG_OPEN_REPARSE_POINT` ensures
        // the reparse data is returned instead of followed;
        // `FILE_FLAG_BACKUP_SEMANTICS` is required to open directories.
        let handle = unsafe {
            CreateFileW(
                windows::core::PCWSTR(wide.as_ptr()),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        };

        let handle = handle.map_err(|_| io::Error::last_os_error())?;
        Ok(OwnedHandle(handle))
    }

    #[test]
    fn junction_is_classified_as_junction() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let target = tmp.path().join("target");
        let junction = tmp.path().join("link");
        fs::create_dir(&target).expect("create target dir");

        let status = Command::new("cmd")
            .args([
                "/c",
                "mklink",
                "/j",
                junction.to_str().expect("utf8 junction path"),
                target.to_str().expect("utf8 target path"),
            ])
            .status();

        let status = match status {
            Ok(s) => s,
            Err(_) => return, // cmd.exe unavailable; skip
        };
        if !status.success() {
            return; // junction creation refused; skip rather than fail
        }

        let handle = open_reparse(&junction).expect("open junction reparse");
        let kind = classify_reparse_point(&handle).expect("classify junction");
        assert_eq!(kind, ReparseKind::Junction);
    }
}
