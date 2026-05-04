//! Flag reading and error marker detection for file list entries.
//!
//! Handles the initial flags byte (or varint) that precedes each entry,
//! including extended flags (protocol 28+), varint-encoded flags, and
//! I/O error markers embedded in the flag stream.

use std::io::{self, Read};

use logging::debug_log;

use crate::CompatibilityFlags;
use crate::varint::read_varint;

use super::FileListReader;
use crate::flist::flags::{FileFlags, XMIT_EXTENDED_FLAGS, XMIT_IO_ERROR_ENDLIST};

/// Result of reading flags from the wire.
///
/// The first byte of each file entry encodes transmission flags that control
/// which metadata fields follow. A zero byte signals end-of-list.
///
/// upstream: flist.c:recv_file_entry() lines 760-780
#[derive(Debug)]
pub enum FlagsResult {
    /// End of file list reached (zero flags byte).
    EndOfList,
    /// I/O error marker with error code from sender.
    IoError(i32),
    /// Valid flags for a file entry.
    Flags(FileFlags),
}

impl FileListReader {
    /// Returns whether varint flag encoding is enabled.
    #[inline]
    pub(super) fn use_varint_flags(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::VARINT_FLIST_FLAGS))
    }

    /// Returns whether safe file list mode is enabled.
    #[inline]
    pub(super) fn use_safe_file_list(&self) -> bool {
        self.compat_flags
            .is_some_and(|f| f.contains(CompatibilityFlags::SAFE_FILE_LIST))
            || self.protocol.safe_file_list_always_enabled()
    }

    /// Reads and validates flags from the wire.
    ///
    /// Returns `FlagsResult::EndOfList` for end-of-list marker,
    /// `FlagsResult::IoError` for I/O error markers, or
    /// `FlagsResult::Flags` for valid entry flags.
    ///
    /// upstream: flist.c:recv_file_entry() lines 2625-2670
    pub(super) fn read_flags<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<FlagsResult> {
        let use_varint = self.use_varint_flags();

        let flags_value = if use_varint {
            read_varint(reader)?
        } else {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0] as i32
        };

        debug_log!(
            Flist,
            4,
            "read_flags: raw={:#x} varint={}",
            flags_value,
            use_varint
        );

        if flags_value == 0 {
            if use_varint {
                // In varint mode, error code follows zero flags
                let io_error = read_varint(reader)?;
                if io_error != 0 {
                    debug_log!(
                        Flist,
                        4,
                        "read_flags: end-of-list with io_error={}",
                        io_error
                    );
                    return Ok(FlagsResult::IoError(io_error));
                }
            }
            debug_log!(Flist, 4, "read_flags: end-of-list marker");
            return Ok(FlagsResult::EndOfList);
        }

        // Read extended flags
        // upstream: flist.c:2628 - extended flags only exist in protocol >= 28.
        // In protocol < 28, bit 2 is XMIT_SAME_RDEV_pre28, not XMIT_EXTENDED_FLAGS.
        let (ext_byte, ext16_byte) = if use_varint {
            (
                ((flags_value >> 8) & 0xFF) as u8,
                ((flags_value >> 16) & 0xFF) as u8,
            )
        } else if self.protocol.as_u8() >= 28 && (flags_value as u8 & XMIT_EXTENDED_FLAGS) != 0 {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            (buf[0], 0u8)
        } else {
            (0u8, 0u8)
        };

        let primary_byte = flags_value as u8;

        if ext_byte != 0 || ext16_byte != 0 {
            debug_log!(
                Flist,
                4,
                "read_flags: primary={:#x} ext={:#x} ext16={:#x}",
                primary_byte,
                ext_byte,
                ext16_byte
            );
        }

        if let Some(error) = self.check_error_marker(primary_byte, ext_byte, reader)? {
            return Ok(FlagsResult::IoError(error));
        }

        // Build flags structure
        let flags = if ext_byte != 0 || ext16_byte != 0 || (primary_byte & XMIT_EXTENDED_FLAGS) != 0
        {
            FileFlags::new_with_extended16(primary_byte, ext_byte, ext16_byte)
        } else {
            FileFlags::new(primary_byte, 0)
        };

        Ok(FlagsResult::Flags(flags))
    }

    /// Checks for I/O error marker in flags.
    ///
    /// In safe file list mode (protocol 31+ or negotiated), the sender can embed
    /// I/O error markers in the flag stream using a two-byte sentinel
    /// (`XMIT_EXTENDED_FLAGS | XMIT_IO_ERROR_ENDLIST << 8`) followed by a varint
    /// error code.
    ///
    /// Returns `Some(error_code)` if an error marker is detected,
    /// `None` if flags represent a valid entry.
    ///
    /// upstream: flist.c:recv_file_entry() safe_flist error check
    fn check_error_marker<R: Read + ?Sized>(
        &self,
        primary: u8,
        extended: u8,
        reader: &mut R,
    ) -> io::Result<Option<i32>> {
        let flags_value = (primary as i32) | ((extended as i32) << 8);
        let error_marker = (XMIT_EXTENDED_FLAGS as i32) | ((XMIT_IO_ERROR_ENDLIST as i32) << 8);

        if flags_value != error_marker {
            return Ok(None);
        }

        if !self.use_safe_file_list() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Invalid flist flag: {flags_value:#x}"),
            ));
        }

        let error_code = read_varint(reader)?;
        Ok(Some(error_code))
    }
}
