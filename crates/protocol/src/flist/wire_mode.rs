/// Platform-normalized mode encoding for the rsync wire protocol.
///
/// On Unix, file mode values use the canonical POSIX bit patterns (e.g., `S_IFLNK = 0o120000`),
/// so these functions are identity operations. On Windows, where `_S_IFLNK` may have a
/// different value, these functions convert between the platform-native representation
/// and the canonical POSIX wire format.
///
/// # Upstream Reference
///
/// - `ifuncs.h:44-63`: `to_wire_mode()` and `from_wire_mode()`

/// Canonical POSIX symlink mode bits used on the wire.
#[cfg(windows)]
const WIRE_S_IFLNK: u32 = 0o120000;

/// Canonical POSIX file type mask.
#[cfg(windows)]
const WIRE_S_IFMT: u32 = 0o170000;

/// Converts a platform-native mode to the wire format.
///
/// On Unix, this is an identity operation since `S_IFLNK` already equals `0o120000`.
/// On Windows, symlink mode bits are normalized to the canonical POSIX value.
#[cfg(not(windows))]
#[inline]
pub fn to_wire_mode(mode: u32) -> i32 {
    mode as i32
}

/// Converts a platform-native mode to the wire format.
///
/// On Windows, `_S_IFLNK` may differ from the POSIX canonical `0o120000`.
/// This function normalizes symlink entries to the wire-standard value.
#[cfg(windows)]
#[inline]
pub fn to_wire_mode(mode: u32) -> i32 {
    // Windows _S_IFLNK is 0xA000 (0o120000 in octal = 40960 in decimal)
    // which happens to match POSIX, but we normalize defensively in case
    // the Windows CRT defines it differently in future toolchains.
    const WINDOWS_S_IFLNK: u32 = 0xA000;
    const WINDOWS_S_IFMT: u32 = 0xF000;

    if WINDOWS_S_IFLNK != WIRE_S_IFLNK && (mode & WINDOWS_S_IFMT) == WINDOWS_S_IFLNK {
        ((mode & !WINDOWS_S_IFLNK) | WIRE_S_IFLNK) as i32
    } else {
        mode as i32
    }
}

/// Converts a wire-format mode back to the platform-native representation.
///
/// On Unix, this is an identity operation since `S_IFLNK` already equals `0o120000`.
/// On Windows, the canonical POSIX symlink bits are converted to the platform-native value.
#[cfg(not(windows))]
#[inline]
pub fn from_wire_mode(mode: i32) -> u32 {
    mode as u32
}

/// Converts a wire-format mode back to the platform-native representation.
///
/// On Windows, the canonical POSIX `0o120000` symlink bits are converted back
/// to the Windows-native `_S_IFLNK` value.
#[cfg(windows)]
#[inline]
pub fn from_wire_mode(mode: i32) -> u32 {
    const WINDOWS_S_IFLNK: u32 = 0xA000;

    let m = mode as u32;
    if WINDOWS_S_IFLNK != WIRE_S_IFLNK
        && (m & (WIRE_S_IFMT & !WINDOWS_S_IFLNK)) == 0
        && (m & WIRE_S_IFLNK) == WIRE_S_IFLNK
    {
        (m & !WIRE_S_IFLNK) | WINDOWS_S_IFLNK
    } else {
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regular_file_mode_round_trips() {
        let mode: u32 = 0o100644;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn directory_mode_round_trips() {
        let mode: u32 = 0o040755;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn symlink_mode_round_trips() {
        let mode: u32 = 0o120777;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn fifo_mode_round_trips() {
        let mode: u32 = 0o010644;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn block_device_mode_round_trips() {
        let mode: u32 = 0o060660;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn char_device_mode_round_trips() {
        let mode: u32 = 0o020660;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn socket_mode_round_trips() {
        let mode: u32 = 0o140755;
        assert_eq!(from_wire_mode(to_wire_mode(mode)), mode);
    }

    #[test]
    fn symlink_wire_format_is_canonical() {
        let mode: u32 = 0o120777;
        let wire = to_wire_mode(mode);
        // Wire format must use canonical POSIX S_IFLNK = 0o120000
        assert_eq!((wire as u32) & 0o170000, 0o120000);
    }

    #[test]
    fn zero_mode_round_trips() {
        assert_eq!(from_wire_mode(to_wire_mode(0)), 0);
    }
}
