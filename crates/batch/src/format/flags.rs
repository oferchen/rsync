//! Stream flags bitmap for batch file headers.
//!
//! The flags encode which rsync options were active during batch creation.
//! Bit positions and protocol-version gating match upstream rsync's
//! `batch.c:write_stream_flags()`.

use std::io::{self, Read, Write};

use super::wire::{read_i32, write_i32};

/// Stream flags bitmap that affects data stream format.
///
/// These flags must match between write and read to ensure correct
/// interpretation of the batch file. The bit positions and protocol-version
/// gating match upstream rsync's `batch.c:59-76 flag_ptr[]` array exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BatchFlags {
    /// Bit 0: --recurse (-r) - upstream: batch.c:60 `&recurse`
    pub recurse: bool,
    /// Bit 1: --owner (-o) - upstream: batch.c:61 `&preserve_uid`
    pub preserve_uid: bool,
    /// Bit 2: --group (-g) - upstream: batch.c:62 `&preserve_gid`
    pub preserve_gid: bool,
    /// Bit 3: --links (-l) - upstream: batch.c:63 `&preserve_links`
    pub preserve_links: bool,
    /// Bit 4: --devices (-D) - upstream: batch.c:64 `&preserve_devices`
    pub preserve_devices: bool,
    /// Bit 5: --hard-links (-H) - upstream: batch.c:65 `&preserve_hard_links`
    pub preserve_hard_links: bool,
    /// Bit 6: --checksum (-c) - upstream: batch.c:66 `&always_checksum`
    pub always_checksum: bool,
    /// Bit 7: --dirs (-d) [protocol >= 29] - upstream: batch.c:67 `&xfer_dirs`
    pub xfer_dirs: bool,
    /// Bit 8: --compress (-z) [protocol >= 29] - upstream: batch.c:68 `&do_compression`
    pub do_compression: bool,
    /// Bit 9: --iconv [protocol >= 30] - upstream: batch.c:69 `&tweaked_iconv`
    pub iconv: bool,
    /// Bit 10: --acls (-A) [protocol >= 30] - upstream: batch.c:70 `&preserve_acls`
    pub preserve_acls: bool,
    /// Bit 11: --xattrs (-X) [protocol >= 30] - upstream: batch.c:71 `&preserve_xattrs`
    pub preserve_xattrs: bool,
    /// Bit 12: --inplace [protocol >= 30] - upstream: batch.c:72 `&inplace`
    pub inplace: bool,
    /// Bit 13: --append [protocol >= 30] - upstream: batch.c:73 `&tweaked_append`
    pub append: bool,
    /// Bit 14: --append-verify [protocol >= 30] - upstream: batch.c:74 `&tweaked_append_verify`
    pub append_verify: bool,
}

impl BatchFlags {
    /// Create a new flags structure from a bitmap.
    #[allow(clippy::field_reassign_with_default)]
    pub fn from_bitmap(bitmap: i32, protocol_version: i32) -> Self {
        let mut flags = Self::default();
        flags.recurse = (bitmap & (1 << 0)) != 0;
        flags.preserve_uid = (bitmap & (1 << 1)) != 0;
        flags.preserve_gid = (bitmap & (1 << 2)) != 0;
        flags.preserve_links = (bitmap & (1 << 3)) != 0;
        flags.preserve_devices = (bitmap & (1 << 4)) != 0;
        flags.preserve_hard_links = (bitmap & (1 << 5)) != 0;
        flags.always_checksum = (bitmap & (1 << 6)) != 0;

        if protocol_version >= 29 {
            flags.xfer_dirs = (bitmap & (1 << 7)) != 0;
            flags.do_compression = (bitmap & (1 << 8)) != 0;
        }

        if protocol_version >= 30 {
            flags.iconv = (bitmap & (1 << 9)) != 0;
            flags.preserve_acls = (bitmap & (1 << 10)) != 0;
            flags.preserve_xattrs = (bitmap & (1 << 11)) != 0;
            flags.inplace = (bitmap & (1 << 12)) != 0;
            flags.append = (bitmap & (1 << 13)) != 0;
            flags.append_verify = (bitmap & (1 << 14)) != 0;
        }

        flags
    }

    /// Convert flags to a bitmap.
    pub const fn to_bitmap(&self, protocol_version: i32) -> i32 {
        let mut bitmap = 0i32;

        if self.recurse {
            bitmap |= 1 << 0;
        }
        if self.preserve_uid {
            bitmap |= 1 << 1;
        }
        if self.preserve_gid {
            bitmap |= 1 << 2;
        }
        if self.preserve_links {
            bitmap |= 1 << 3;
        }
        if self.preserve_devices {
            bitmap |= 1 << 4;
        }
        if self.preserve_hard_links {
            bitmap |= 1 << 5;
        }
        if self.always_checksum {
            bitmap |= 1 << 6;
        }

        if protocol_version >= 29 {
            if self.xfer_dirs {
                bitmap |= 1 << 7;
            }
            if self.do_compression {
                bitmap |= 1 << 8;
            }
        }

        if protocol_version >= 30 {
            if self.iconv {
                bitmap |= 1 << 9;
            }
            if self.preserve_acls {
                bitmap |= 1 << 10;
            }
            if self.preserve_xattrs {
                bitmap |= 1 << 11;
            }
            if self.inplace {
                bitmap |= 1 << 12;
            }
            if self.append {
                bitmap |= 1 << 13;
            }
            if self.append_verify {
                bitmap |= 1 << 14;
            }
        }

        bitmap
    }

    /// Write flags to a writer, masking bits by protocol version.
    ///
    /// Only bits valid for the given protocol version are written.
    /// Upstream `batch.c:write_stream_flags()` uses the negotiated
    /// `protocol_version` to decide which bits to set.
    pub fn write_to_versioned<W: Write>(
        &self,
        writer: &mut W,
        protocol_version: i32,
    ) -> io::Result<()> {
        write_i32(writer, self.to_bitmap(protocol_version))
    }

    /// Read the raw bitmap from a reader.
    ///
    /// Returns the raw `i32` bitmap without interpreting protocol-gated bits.
    /// The caller must pass this to [`BatchFlags::from_bitmap`] with the
    /// correct protocol version (read from the header after the bitmap).
    pub fn read_raw<R: Read>(reader: &mut R) -> io::Result<i32> {
        read_i32(reader)
    }
}
