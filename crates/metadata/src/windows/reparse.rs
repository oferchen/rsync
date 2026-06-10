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
//! through.
//!
//! In addition to classification, the module exposes the per-kind payload
//! parsers [`parse_symlink_reparse`], [`parse_junction_reparse`], and
//! [`parse_mount_point_reparse`]. Each parser validates the buffer against
//! the documented `SYMBOLIC_LINK_REPARSE_BUFFER` / `MOUNT_POINT_REPARSE_BUFFER`
//! layout from `winnt.h` and returns the substitute and print names as
//! [`OsString`] values plus, for symlinks, the `SYMLINK_FLAG_RELATIVE`
//! bit. Cloud and `AF_UNIX` payloads are not parsed because they carry
//! opaque per-provider blobs.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
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

/// `PrintNameOffset` field offset inside `MountPointReparseBuffer`.
const MOUNT_POINT_PRINT_NAME_OFFSET: usize = REPARSE_HEADER_SIZE + 4;
/// `PrintNameLength` field offset inside `MountPointReparseBuffer`.
const MOUNT_POINT_PRINT_NAME_LENGTH: usize = REPARSE_HEADER_SIZE + 6;

/// `SubstituteNameOffset` field offset inside the `SymbolicLinkReparseBuffer`
/// payload (immediately following the common header).
const SYMLINK_SUBSTITUTE_NAME_OFFSET: usize = REPARSE_HEADER_SIZE;
/// `SubstituteNameLength` field offset inside `SymbolicLinkReparseBuffer`.
const SYMLINK_SUBSTITUTE_NAME_LENGTH: usize = REPARSE_HEADER_SIZE + 2;
/// `PrintNameOffset` field offset inside `SymbolicLinkReparseBuffer`.
const SYMLINK_PRINT_NAME_OFFSET: usize = REPARSE_HEADER_SIZE + 4;
/// `PrintNameLength` field offset inside `SymbolicLinkReparseBuffer`.
const SYMLINK_PRINT_NAME_LENGTH: usize = REPARSE_HEADER_SIZE + 6;
/// `Flags` field offset (u32) inside `SymbolicLinkReparseBuffer`.
const SYMLINK_FLAGS_OFFSET: usize = REPARSE_HEADER_SIZE + 8;
/// Offset of the variable-length `PathBuffer` inside
/// `SymbolicLinkReparseBuffer` (after the four `u16` offset/length fields
/// and the `u32` flags word).
const SYMLINK_PATH_BUFFER_OFFSET: usize = REPARSE_HEADER_SIZE + 12;

/// `SYMLINK_FLAG_RELATIVE` from `winnt.h`. Set when the substitute-name is
/// a relative path; cleared when it is an absolute NT-namespace path.
///
/// `windows::Win32::System::Ioctl` does not expose this constant directly,
/// so it is pinned here against the documented value.
const SYMLINK_FLAG_RELATIVE: u32 = 0x0000_0001;

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
    if buf.len() < payload_end
        || data_length < (MOUNT_POINT_PATH_BUFFER_OFFSET - REPARSE_HEADER_SIZE)
    {
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

/// Parsed payload of an `IO_REPARSE_TAG_SYMLINK` reparse buffer.
///
/// Mirrors the documented `SymbolicLinkReparseBuffer` shape from `winnt.h`:
///
/// ```text
/// USHORT SubstituteNameOffset;
/// USHORT SubstituteNameLength;
/// USHORT PrintNameOffset;
/// USHORT PrintNameLength;
/// ULONG  Flags;
/// WCHAR  PathBuffer[1];
/// ```
///
/// The substitute and print names are stored as UTF-16LE without a
/// trailing null. `is_relative` reflects the `SYMLINK_FLAG_RELATIVE`
/// (`0x00000001`) bit in `Flags`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SymlinkReparseData {
    /// NT-namespace target as stored on disk, decoded from the
    /// substitute-name UTF-16LE slice. For absolute links this carries the
    /// `\??\` prefix; for relative links it carries the bare relative
    /// path.
    pub substitute_name: OsString,
    /// User-facing target as stored on disk, decoded from the print-name
    /// UTF-16LE slice. May be empty when the kernel omitted a print name.
    pub print_name: OsString,
    /// `true` when the link is relative (`SYMLINK_FLAG_RELATIVE` set in
    /// `Flags`); `false` for absolute NT-namespace targets.
    pub is_relative: bool,
}

/// Parsed payload of a directory junction (non-volume
/// `IO_REPARSE_TAG_MOUNT_POINT`) reparse buffer.
///
/// Junctions and volume mount-points share the on-disk
/// `MountPointReparseBuffer` layout; the substitute-name prefix
/// distinguishes them. See [`classify_mount_point`] for the split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JunctionReparseData {
    /// NT-namespace target as stored on disk, typically `\??\C:\target`.
    pub substitute_name: OsString,
    /// User-facing target as stored on disk, typically `C:\target`. May
    /// be empty when the kernel omitted a print name.
    pub print_name: OsString,
}

/// Parsed payload of a volume mount-point (volume-prefixed
/// `IO_REPARSE_TAG_MOUNT_POINT`) reparse buffer.
///
/// Same on-disk shape as [`JunctionReparseData`]; the substitute-name
/// carries the `\??\Volume{GUID}\` NT-namespace prefix when the entry
/// is a volume mount-point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountPointReparseData {
    /// NT-namespace volume path. Expected to start with the
    /// `\??\Volume{GUID}\` prefix; callers may treat this as the source
    /// of truth for the mounted volume identity.
    pub volume_guid_path: OsString,
    /// User-facing mount-point target as stored on disk. May be empty.
    pub print_name: OsString,
}

/// Parse the payload of an `IO_REPARSE_TAG_SYMLINK` reparse buffer.
///
/// `buf` must hold the full `REPARSE_DATA_BUFFER` (header + payload) as
/// returned by `FSCTL_GET_REPARSE_POINT`. Validates the `ReparseTag`,
/// `ReparseDataLength`, the four `u16` offset/length fields, the `u32`
/// flags word, and the substitute/print-name slices before decoding the
/// UTF-16LE bytes into [`OsString`] via [`OsStringExt::from_wide`].
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidData`] when:
/// - the buffer is shorter than the documented minimum header/payload size,
/// - the tag is not `IO_REPARSE_TAG_SYMLINK`,
/// - `ReparseDataLength` declares more bytes than `buf` carries,
/// - any substitute/print-name offset/length pair runs past the payload,
/// - any substitute/print-name length is not a multiple of two bytes.
pub(crate) fn parse_symlink_reparse(buf: &[u8]) -> io::Result<SymlinkReparseData> {
    validate_tag(buf, IO_REPARSE_TAG_SYMLINK)?;

    let payload_end = validate_payload_bounds(buf, SYMLINK_PATH_BUFFER_OFFSET)?;

    let substitute_offset = read_u16_le(buf, SYMLINK_SUBSTITUTE_NAME_OFFSET) as usize;
    let substitute_length = read_u16_le(buf, SYMLINK_SUBSTITUTE_NAME_LENGTH) as usize;
    let print_offset = read_u16_le(buf, SYMLINK_PRINT_NAME_OFFSET) as usize;
    let print_length = read_u16_le(buf, SYMLINK_PRINT_NAME_LENGTH) as usize;
    let flags = read_u32_le(buf, SYMLINK_FLAGS_OFFSET);

    let substitute_name = decode_utf16_name(
        buf,
        SYMLINK_PATH_BUFFER_OFFSET,
        substitute_offset,
        substitute_length,
        payload_end,
        "symlink substitute name",
    )?;
    let print_name = decode_utf16_name(
        buf,
        SYMLINK_PATH_BUFFER_OFFSET,
        print_offset,
        print_length,
        payload_end,
        "symlink print name",
    )?;

    Ok(SymlinkReparseData {
        substitute_name,
        print_name,
        is_relative: (flags & SYMLINK_FLAG_RELATIVE) != 0,
    })
}

/// Parse the payload of a directory junction reparse buffer.
///
/// Junctions use `IO_REPARSE_TAG_MOUNT_POINT` with a non-volume
/// substitute-name prefix. This parser only validates the tag and
/// layout; the caller is expected to call [`classify_reparse_point`] or
/// [`classify_from_buffer`] first to distinguish a junction from a
/// volume mount-point.
///
/// # Errors
///
/// Same conditions as [`parse_symlink_reparse`], but for the
/// `MountPointReparseBuffer` shape (no flags word).
pub(crate) fn parse_junction_reparse(buf: &[u8]) -> io::Result<JunctionReparseData> {
    let (substitute_name, print_name) = parse_mount_point_layout(buf)?;
    Ok(JunctionReparseData {
        substitute_name,
        print_name,
    })
}

/// Parse the payload of a volume mount-point reparse buffer.
///
/// Volume mount-points share the on-disk layout of directory junctions
/// (`IO_REPARSE_TAG_MOUNT_POINT`) but carry a `\??\Volume{GUID}\`
/// substitute-name prefix. The parser returns the substitute-name in
/// `volume_guid_path` for callers that want to extract the volume GUID.
///
/// # Errors
///
/// Same conditions as [`parse_junction_reparse`].
pub(crate) fn parse_mount_point_reparse(buf: &[u8]) -> io::Result<MountPointReparseData> {
    let (volume_guid_path, print_name) = parse_mount_point_layout(buf)?;
    Ok(MountPointReparseData {
        volume_guid_path,
        print_name,
    })
}

/// Shared validator+decoder for the `MountPointReparseBuffer` shape used
/// by both junctions and volume mount-points.
///
/// Returns `(substitute_name, print_name)` as decoded [`OsString`]s.
fn parse_mount_point_layout(buf: &[u8]) -> io::Result<(OsString, OsString)> {
    validate_tag(buf, IO_REPARSE_TAG_MOUNT_POINT)?;

    let payload_end = validate_payload_bounds(buf, MOUNT_POINT_PATH_BUFFER_OFFSET)?;

    let substitute_offset = read_u16_le(buf, MOUNT_POINT_SUBSTITUTE_NAME_OFFSET) as usize;
    let substitute_length = read_u16_le(buf, MOUNT_POINT_SUBSTITUTE_NAME_LENGTH) as usize;
    let print_offset = read_u16_le(buf, MOUNT_POINT_PRINT_NAME_OFFSET) as usize;
    let print_length = read_u16_le(buf, MOUNT_POINT_PRINT_NAME_LENGTH) as usize;

    let substitute_name = decode_utf16_name(
        buf,
        MOUNT_POINT_PATH_BUFFER_OFFSET,
        substitute_offset,
        substitute_length,
        payload_end,
        "mount-point substitute name",
    )?;
    let print_name = decode_utf16_name(
        buf,
        MOUNT_POINT_PATH_BUFFER_OFFSET,
        print_offset,
        print_length,
        payload_end,
        "mount-point print name",
    )?;
    Ok((substitute_name, print_name))
}

/// Validate that `buf` carries the documented `REPARSE_DATA_BUFFER`
/// header and that `ReparseTag` matches `expected`.
fn validate_tag(buf: &[u8], expected: u32) -> io::Result<()> {
    if buf.len() < REPARSE_HEADER_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "reparse buffer too short: {} bytes, expected at least {}",
                buf.len(),
                REPARSE_HEADER_SIZE
            ),
        ));
    }
    let tag = read_u32_le(buf, REPARSE_TAG_OFFSET);
    if tag != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected reparse tag {tag:#010x}, expected {expected:#010x}"),
        ));
    }
    Ok(())
}

/// Validate that `ReparseDataLength` fits inside `buf` and that the
/// payload covers at least up to `path_buffer_offset` (the start of the
/// variable-length name area).
///
/// Returns the absolute end offset of the payload inside `buf` so the
/// name decoders can clamp their slices.
fn validate_payload_bounds(buf: &[u8], path_buffer_offset: usize) -> io::Result<usize> {
    let data_length = read_u16_le(buf, REPARSE_DATA_LENGTH_OFFSET) as usize;
    let payload_end = REPARSE_HEADER_SIZE
        .checked_add(data_length)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("ReparseDataLength {data_length} overflows usize"),
            )
        })?;
    if payload_end > buf.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "ReparseDataLength {data_length} runs past buffer (buf {}, payload_end {})",
                buf.len(),
                payload_end
            ),
        ));
    }
    if payload_end < path_buffer_offset {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "reparse payload {data_length} bytes shorter than minimum header {}",
                path_buffer_offset - REPARSE_HEADER_SIZE
            ),
        ));
    }
    Ok(payload_end)
}

/// Decode a UTF-16LE name slice out of a `REPARSE_DATA_BUFFER` payload.
///
/// `path_buffer_offset` is the absolute offset of `PathBuffer` inside
/// `buf`. `name_offset` and `name_length` are the per-name offset and
/// length values read out of the payload's `u16` fields, both in bytes.
/// `payload_end` is the absolute end of the declared payload inside
/// `buf` and is used to clamp the name slice.
///
/// Zero-length names decode to an empty [`OsString`]; non-zero lengths
/// that are not a multiple of two bytes or that run past the payload
/// surface as [`io::ErrorKind::InvalidData`] with the supplied `label`
/// in the message.
fn decode_utf16_name(
    buf: &[u8],
    path_buffer_offset: usize,
    name_offset: usize,
    name_length: usize,
    payload_end: usize,
    label: &str,
) -> io::Result<OsString> {
    if name_length == 0 {
        return Ok(OsString::new());
    }
    if name_length % size_of::<u16>() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} length {name_length} not a multiple of 2"),
        ));
    }
    let name_start = path_buffer_offset.checked_add(name_offset).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} offset {name_offset} overflows usize"),
        )
    })?;
    let name_end = name_start.checked_add(name_length).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} length {name_length} overflows usize"),
        )
    })?;
    if name_end > payload_end {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{label} runs past payload (name_end {name_end}, payload_end {payload_end})"
            ),
        ));
    }
    let wide: Vec<u16> = buf[name_start..name_end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(OsString::from_wide(&wide))
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
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
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
        let buf = build_mount_point_buffer("\\??\\Volume{12345678-1234-1234-1234-1234567890ab}\\");
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

    /// Build a synthetic `SymbolicLinkReparseBuffer` with the supplied
    /// substitute and print names laid out back-to-back inside
    /// `PathBuffer` with `u16` null-terminators between them.
    fn build_symlink_buffer(substitute: &str, print: &str, flags: u32) -> Vec<u8> {
        let sub_utf16 = utf16_le(substitute);
        let print_utf16 = utf16_le(print);
        let sub_len = sub_utf16.len();
        let print_len = print_utf16.len();
        let path_buffer_len = sub_len + 2 + print_len + 2;
        // 4 u16 offset/length fields + 1 u32 flags + variable PathBuffer.
        let data_length = 8 + 4 + path_buffer_len;

        let mut buf = Vec::with_capacity(REPARSE_HEADER_SIZE + data_length);
        buf.extend_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
        buf.extend_from_slice(&(data_length as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset = 0
        buf.extend_from_slice(&(sub_len as u16).to_le_bytes()); // SubstituteNameLength
        buf.extend_from_slice(&((sub_len + 2) as u16).to_le_bytes()); // PrintNameOffset
        buf.extend_from_slice(&(print_len as u16).to_le_bytes()); // PrintNameLength
        buf.extend_from_slice(&flags.to_le_bytes()); // Flags
        buf.extend_from_slice(&sub_utf16);
        buf.extend_from_slice(&[0, 0]); // sub-name null-term
        buf.extend_from_slice(&print_utf16);
        buf.extend_from_slice(&[0, 0]); // print-name null-term
        buf
    }

    /// Build a synthetic `MountPointReparseBuffer` with both substitute
    /// and print names populated.
    fn build_mount_point_buffer_with_print(substitute: &str, print: &str) -> Vec<u8> {
        let sub_utf16 = utf16_le(substitute);
        let print_utf16 = utf16_le(print);
        let sub_len = sub_utf16.len();
        let print_len = print_utf16.len();
        let path_buffer_len = sub_len + 2 + print_len + 2;
        let data_length = 8 + path_buffer_len;

        let mut buf = Vec::with_capacity(REPARSE_HEADER_SIZE + data_length);
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        buf.extend_from_slice(&(data_length as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf.extend_from_slice(&0u16.to_le_bytes()); // SubstituteNameOffset = 0
        buf.extend_from_slice(&(sub_len as u16).to_le_bytes());
        buf.extend_from_slice(&((sub_len + 2) as u16).to_le_bytes());
        buf.extend_from_slice(&(print_len as u16).to_le_bytes());
        buf.extend_from_slice(&sub_utf16);
        buf.extend_from_slice(&[0, 0]);
        buf.extend_from_slice(&print_utf16);
        buf.extend_from_slice(&[0, 0]);
        buf
    }

    #[test]
    fn parses_relative_symlink() {
        let buf = build_symlink_buffer("..\\target", "..\\target", SYMLINK_FLAG_RELATIVE);
        let parsed = parse_symlink_reparse(&buf).expect("parse relative symlink");
        assert_eq!(parsed.substitute_name, OsString::from("..\\target"));
        assert_eq!(parsed.print_name, OsString::from("..\\target"));
        assert!(parsed.is_relative);
    }

    #[test]
    fn parses_absolute_symlink() {
        let buf = build_symlink_buffer("\\??\\C:\\target", "C:\\target", 0);
        let parsed = parse_symlink_reparse(&buf).expect("parse absolute symlink");
        assert_eq!(parsed.substitute_name, OsString::from("\\??\\C:\\target"));
        assert_eq!(parsed.print_name, OsString::from("C:\\target"));
        assert!(!parsed.is_relative);
    }

    #[test]
    fn parses_symlink_with_empty_print_name() {
        let buf = build_symlink_buffer("\\??\\C:\\target", "", SYMLINK_FLAG_RELATIVE);
        let parsed = parse_symlink_reparse(&buf).expect("parse symlink with empty print");
        assert_eq!(parsed.substitute_name, OsString::from("\\??\\C:\\target"));
        assert_eq!(parsed.print_name, OsString::new());
        assert!(parsed.is_relative);
    }

    #[test]
    fn rejects_symlink_with_wrong_tag() {
        let mut buf = build_symlink_buffer("..\\x", "..\\x", SYMLINK_FLAG_RELATIVE);
        // Overwrite the tag with IO_REPARSE_TAG_MOUNT_POINT.
        buf[..4].copy_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        let err = parse_symlink_reparse(&buf).expect_err("wrong tag must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_truncated_symlink_buffer() {
        let buf = build_symlink_buffer("..\\target", "..\\target", SYMLINK_FLAG_RELATIVE);
        // Truncate inside the header to force a length check.
        let err = parse_symlink_reparse(&buf[..REPARSE_HEADER_SIZE - 1])
            .expect_err("truncated header must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_symlink_with_oversized_data_length() {
        let mut buf = build_symlink_buffer("..\\x", "..\\x", SYMLINK_FLAG_RELATIVE);
        // Bump ReparseDataLength past the actual buffer length.
        let oversized = (buf.len() as u16).saturating_add(64);
        buf[REPARSE_DATA_LENGTH_OFFSET..REPARSE_DATA_LENGTH_OFFSET + 2]
            .copy_from_slice(&oversized.to_le_bytes());
        let err = parse_symlink_reparse(&buf).expect_err("oversized data length must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parses_junction() {
        let buf = build_mount_point_buffer_with_print("\\??\\C:\\target", "C:\\target");
        let parsed = parse_junction_reparse(&buf).expect("parse junction");
        assert_eq!(parsed.substitute_name, OsString::from("\\??\\C:\\target"));
        assert_eq!(parsed.print_name, OsString::from("C:\\target"));
    }

    #[test]
    fn rejects_junction_with_wrong_tag() {
        let mut buf = build_mount_point_buffer_with_print("\\??\\C:\\target", "C:\\target");
        buf[..4].copy_from_slice(&IO_REPARSE_TAG_SYMLINK.to_le_bytes());
        let err = parse_junction_reparse(&buf).expect_err("wrong tag must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parses_mount_point() {
        let buf = build_mount_point_buffer_with_print(
            "\\??\\Volume{12345678-1234-1234-1234-123456789012}\\",
            "",
        );
        let parsed = parse_mount_point_reparse(&buf).expect("parse mount-point");
        assert_eq!(
            parsed.volume_guid_path,
            OsString::from("\\??\\Volume{12345678-1234-1234-1234-123456789012}\\")
        );
        assert_eq!(parsed.print_name, OsString::new());
    }

    #[test]
    fn rejects_truncated_mount_point_buffer() {
        let buf = build_mount_point_buffer_with_print("\\??\\C:\\x", "C:\\x");
        let err = parse_junction_reparse(&buf[..REPARSE_HEADER_SIZE])
            .expect_err("truncated payload must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parses_mount_point_with_zero_length_names() {
        // Build a minimal valid layout where both name lengths are zero;
        // both substitute_name and print_name should decode to empty.
        let mut buf = Vec::new();
        buf.extend_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
        // ReparseDataLength = 8 (four u16 fields) + 4 (PathBuffer terminators)
        buf.extend_from_slice(&12u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // Reserved
        buf.extend_from_slice(&[0u8; 8]); // four u16 fields all zero
        buf.extend_from_slice(&[0u8; 4]); // empty PathBuffer terminators

        let parsed = parse_junction_reparse(&buf).expect("zero-length names parse");
        assert!(parsed.substitute_name.is_empty());
        assert!(parsed.print_name.is_empty());
    }

    #[test]
    fn rejects_symlink_with_odd_name_length() {
        let mut buf = build_symlink_buffer("..\\x", "..\\x", SYMLINK_FLAG_RELATIVE);
        // SubstituteNameLength sits at REPARSE_HEADER_SIZE + 2 = offset 10.
        let bad = 3u16; // odd, not a multiple of 2
        buf[SYMLINK_SUBSTITUTE_NAME_LENGTH..SYMLINK_SUBSTITUTE_NAME_LENGTH + 2]
            .copy_from_slice(&bad.to_le_bytes());
        let err = parse_symlink_reparse(&buf).expect_err("odd UTF-16 length must error");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
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

    #[test]
    fn junction_parse_extracts_target_path() {
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
        let buf = fetch_reparse_buffer(&handle).expect("read reparse buffer");
        assert_eq!(classify_from_buffer(&buf), ReparseKind::Junction);
        let parsed = parse_junction_reparse(&buf).expect("parse junction reparse");

        // The kernel stores the NT-namespace form (\??\<path>) in the
        // substitute-name; the user-facing form lives in the print-name.
        // We only assert that the target path is preserved in one of the
        // two name fields to keep the test resilient to the canonical-form
        // normalisation Windows applies (e.g. trailing slashes, \\?\ vs
        // \??\). The target string is compared as wide chars so that
        // case-insensitive volume letters do not flip the result.
        let target_str = target.to_str().expect("utf8 target path");
        let substitute = parsed.substitute_name.to_string_lossy().into_owned();
        let print = parsed.print_name.to_string_lossy().into_owned();
        let found = substitute.eq_ignore_ascii_case(target_str)
            || substitute
                .trim_start_matches("\\??\\")
                .eq_ignore_ascii_case(target_str)
            || print.eq_ignore_ascii_case(target_str)
            || print
                .trim_start_matches("\\??\\")
                .eq_ignore_ascii_case(target_str);
        assert!(
            found,
            "junction target not preserved (substitute={substitute:?}, print={print:?}, target={target_str:?})"
        );
    }

    /// Fetch the raw `REPARSE_DATA_BUFFER` bytes for an open reparse-point
    /// handle so the parser can be exercised in isolation from the
    /// classifier wrapper.
    fn fetch_reparse_buffer(handle: &OwnedHandle) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE];
        let mut bytes_returned: u32 = 0;
        let raw = handle.as_raw_handle();
        let h = HANDLE(raw.cast());

        // SAFETY: `h` is a valid Win32 file handle borrowed for the call;
        // `buffer` is heap-owned at the documented maximum payload size;
        // `bytes_returned` is a valid stack pointer; no input buffer is
        // required for `FSCTL_GET_REPARSE_POINT`.
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
        buffer.truncate(bytes_returned as usize);
        Ok(buffer)
    }
}
