//! Item flags for transfer requirements and itemize output.
//!
//! Defines the 16-bit wire flags and 3 log-only upper bits that encode per-file
//! transfer state. Used by both the generator (reading receiver requests) and
//! the receiver (emitting itemize output via MSG_INFO).
//!
//! # Upstream Reference
//!
//! - `rsync.h:214-236` - `ITEM_*` flag definitions and `SIGNIFICANT_ITEM_FLAGS`
//! - `rsync.c:227` - `read_ndx_and_attrs()` reads iflags from wire
//! - `log.c:695-746` - `%i` expansion uses these flags for itemize output

use std::io::{self, Read};

use crate::role_trailer::error_location;

/// Item flags received from the receiver indicating transfer requirements.
///
/// The generator reads these flags to determine how to handle each file request.
/// Protocol versions >= 29 include these flags with each file index as a 16-bit
/// wire value. Bits 16-18 are internal-only for log formatting and never sent on wire.
///
/// # Upstream Reference
///
/// - `rsync.h:214-233` - Item flag definitions
/// - `rsync.c:227` - `read_ndx_and_attrs()` reads iflags
/// - `sender.c:324` - Sender processes these flags
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemFlags {
    /// Raw flags value. Lower 16 bits are on-wire; bits 16-18 are log-only.
    raw: u32,
}

impl ItemFlags {
    // Wire flags (bits 0-15) - upstream rsync.h:214-229
    /// Item reports access time change.
    pub const ITEM_REPORT_ATIME: u32 = 1 << 0; // 0x0001
    /// Item reports generic change (itemized output).
    pub const ITEM_REPORT_CHANGE: u32 = 1 << 1; // 0x0002
    /// Item reports size change (regular files only).
    pub const ITEM_REPORT_SIZE: u32 = 1 << 2; // 0x0004
    /// Item reports symlink time-set failure (symlinks only, shares bit with `ITEM_REPORT_SIZE`).
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h:217` - `ITEM_REPORT_TIMEFAIL (1<<2) /* symlinks only */`
    /// - `log.c:709-710` - checked to emit `T` instead of `t` for symlink time position
    pub const ITEM_REPORT_TIMEFAIL: u32 = 1 << 2; // 0x0004
    /// Item reports mtime change.
    pub const ITEM_REPORT_TIME: u32 = 1 << 3; // 0x0008
    /// Item reports permissions change.
    pub const ITEM_REPORT_PERMS: u32 = 1 << 4; // 0x0010
    /// Item reports owner change.
    pub const ITEM_REPORT_OWNER: u32 = 1 << 5; // 0x0020
    /// Item reports group change.
    pub const ITEM_REPORT_GROUP: u32 = 1 << 6; // 0x0040
    /// Item reports ACL change.
    pub const ITEM_REPORT_ACL: u32 = 1 << 7; // 0x0080
    /// Item reports xattr change.
    pub const ITEM_REPORT_XATTR: u32 = 1 << 8; // 0x0100
    // bit 9 unused in upstream
    /// Item reports creation time change.
    pub const ITEM_REPORT_CRTIME: u32 = 1 << 10; // 0x0400
    /// Basis file type follows on wire.
    pub const ITEM_BASIS_TYPE_FOLLOWS: u32 = 1 << 11; // 0x0800
    /// Alternate basis file name follows on wire.
    pub const ITEM_XNAME_FOLLOWS: u32 = 1 << 12; // 0x1000
    /// Item is newly created.
    pub const ITEM_IS_NEW: u32 = 1 << 13; // 0x2000
    /// Item has local change (e.g. fuzzy match).
    pub const ITEM_LOCAL_CHANGE: u32 = 1 << 14; // 0x4000
    /// Item needs data transfer (file content differs).
    pub const ITEM_TRANSFER: u32 = 1 << 15; // 0x8000

    // Log-only flags (bits 16-18) - never sent on wire
    /// Item is missing data (log formatting only).
    pub const ITEM_MISSING_DATA: u32 = 1 << 16; // 0x1_0000
    /// Item was deleted (log formatting only).
    pub const ITEM_DELETED: u32 = 1 << 17; // 0x2_0000
    /// Item was matched (log formatting only).
    pub const ITEM_MATCHED: u32 = 1 << 18; // 0x4_0000

    /// Bitmask of flags considered significant for wire transmission and itemize
    /// reporting. Strips framing-only bits (`ITEM_BASIS_TYPE_FOLLOWS`,
    /// `ITEM_XNAME_FOLLOWS`) and the local-only `ITEM_LOCAL_CHANGE` flag.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h:235-236` - `SIGNIFICANT_ITEM_FLAGS`
    pub const SIGNIFICANT_ITEM_FLAGS: u32 =
        !(Self::ITEM_BASIS_TYPE_FOLLOWS | Self::ITEM_XNAME_FOLLOWS | Self::ITEM_LOCAL_CHANGE);

    /// Creates ItemFlags from a raw value.
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self { raw }
    }

    /// Returns the raw flags value (including log-only upper bits).
    #[must_use]
    pub const fn raw(&self) -> u32 {
        self.raw
    }

    /// Returns the lower 16 bits suitable for wire transmission.
    #[must_use]
    pub const fn wire_bits(&self) -> u16 {
        self.raw as u16
    }

    /// Returns the wire-format value with only significant flags preserved.
    ///
    /// Applies [`Self::SIGNIFICANT_ITEM_FLAGS`] mask, then truncates to 16 bits.
    /// Use this when sending iflags on the wire to strip framing-only and
    /// internal-only bits that should not leak to the remote side.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.h:235-236` - `SIGNIFICANT_ITEM_FLAGS` definition
    /// - `generator.c:574-575` - Applied before wire send in `itemize()`
    #[must_use]
    pub const fn significant_wire_bits(&self) -> u16 {
        (self.raw & Self::SIGNIFICANT_ITEM_FLAGS) as u16
    }

    /// Returns true if the item needs data transfer.
    #[must_use]
    pub const fn needs_transfer(&self) -> bool {
        self.raw & Self::ITEM_TRANSFER != 0
    }

    /// Returns true if basis file type follows.
    #[must_use]
    pub const fn has_basis_type(&self) -> bool {
        self.raw & Self::ITEM_BASIS_TYPE_FOLLOWS != 0
    }

    /// Returns true if extended name follows.
    #[must_use]
    pub const fn has_xname(&self) -> bool {
        self.raw & Self::ITEM_XNAME_FOLLOWS != 0
    }

    /// Reads item flags from the wire.
    ///
    /// For protocol >= 29, reads 2 bytes little-endian (16-bit wire format).
    /// For older protocols, returns ITEM_TRANSFER as default.
    pub fn read<R: Read>(reader: &mut R, protocol_version: u8) -> io::Result<Self> {
        if protocol_version >= 29 {
            let mut buf = [0u8; 2];
            reader.read_exact(&mut buf)?;
            Ok(Self::from_raw(u16::from_le_bytes(buf) as u32))
        } else {
            Ok(Self::from_raw(Self::ITEM_TRANSFER))
        }
    }

    /// Reads optional trailing fields based on flags.
    ///
    /// Returns `(fnamecmp_type, xname)` where each is present only if indicated by flags.
    /// The `fnamecmp_type` is decoded into a typed `FnameCmpType` enum that mirrors
    /// upstream `FNAMECMP_*` constants from `rsync.h`.
    pub fn read_trailing<R: Read>(
        &self,
        reader: &mut R,
    ) -> io::Result<(Option<protocol::FnameCmpType>, Option<Vec<u8>>)> {
        let fnamecmp_type = if self.has_basis_type() {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Some(protocol::FnameCmpType::from_wire(byte[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "invalid fnamecmp type: 0x{:02X} {}{}",
                        byte[0],
                        error_location!(),
                        crate::role_trailer::sender()
                    ),
                )
            })?)
        } else {
            None
        };

        let xname = if self.has_xname() {
            let xlen = protocol::read_varint(reader)? as usize;
            if xlen > 0 {
                let actual_len = xlen.min(4096);
                let mut xname_buf = vec![0u8; actual_len];
                reader.read_exact(&mut xname_buf)?;
                Some(xname_buf)
            } else {
                None
            }
        } else {
            None
        };

        Ok((fnamecmp_type, xname))
    }
}
