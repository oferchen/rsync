//! Wire encoding for flags, names, symlinks, devices, hardlinks, and checksums.
//!
//! Handles the low-level serialization of individual fields to the rsync
//! wire format, including protocol-version-specific encoding differences.

use std::io::{self, Write};

use crate::codec::ProtocolCodec;
use crate::varint::{write_varint, write_varint30_int};

use super::super::entry::FileEntry;
use super::super::flags::{
    XMIT_EXTENDED_FLAGS, XMIT_HLINK_FIRST, XMIT_HLINKED, XMIT_IO_ERROR_ENDLIST, XMIT_LONG_NAME,
    XMIT_RDEV_MINOR_8_PRE30, XMIT_SAME_DEV_PRE30, XMIT_SAME_NAME, XMIT_SAME_RDEV_MAJOR,
    XMIT_TOP_DIR,
};
use super::super::wire_path::path_bytes_to_wire;
use super::FileListWriter;

impl FileListWriter {
    /// Writes flags to the wire in the appropriate format.
    ///
    /// Three encoding modes depending on protocol and negotiated capabilities:
    /// - Varint mode (VARINT_FLIST_FLAGS): single varint, zero avoided
    /// - Protocol 28+: one or two bytes depending on extended flags
    /// - Protocol < 28: single byte
    ///
    /// // upstream: flist.c:send_file_entry() lines 545-565
    pub(super) fn write_flags<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        xflags: u32,
        is_dir: bool,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: avoid xflags=0 which looks like end marker.
            // Upstream flist.c line 550: write_varint(f, xflags ? xflags : XMIT_EXTENDED_FLAGS)
            let flags_to_write = if xflags == 0 {
                XMIT_EXTENDED_FLAGS as u32
            } else {
                xflags
            };
            write_varint(writer, flags_to_write as i32)?;
        } else if self.protocol.supports_extended_flags() {
            // Protocol 28-29: two-byte encoding if needed
            let mut xflags_to_write = xflags;
            if xflags_to_write == 0 && !is_dir {
                xflags_to_write |= XMIT_TOP_DIR as u32;
            }

            if (xflags_to_write & 0xFF00) != 0 || xflags_to_write == 0 {
                xflags_to_write |= XMIT_EXTENDED_FLAGS as u32;
                writer.write_all(&(xflags_to_write as u16).to_le_bytes())?;
            } else {
                writer.write_all(&[xflags_to_write as u8])?;
            }
        } else {
            // Protocol < 28: single byte
            // upstream: flist.c:559-562 - dirs use XMIT_LONG_NAME, non-dirs use XMIT_TOP_DIR
            let flags_to_write = if (xflags & 0xFF) == 0 {
                if is_dir {
                    xflags | XMIT_LONG_NAME as u32
                } else {
                    xflags | XMIT_TOP_DIR as u32
                }
            } else {
                xflags
            };
            writer.write_all(&[flags_to_write as u8])?;
        }
        Ok(())
    }

    /// Writes name compression info and suffix.
    ///
    /// Encodes the shared prefix length (if `XMIT_SAME_NAME`) and the name
    /// suffix. Long names (> 255 bytes) use varint length encoding.
    ///
    /// // upstream: flist.c:send_file_entry() lines 566-580
    pub(super) fn write_name<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        name: &[u8],
        same_len: usize,
        suffix_len: usize,
        xflags: u32,
    ) -> io::Result<()> {
        if xflags & (XMIT_SAME_NAME as u32) != 0 {
            writer.write_all(&[same_len as u8])?;
        }

        if xflags & (XMIT_LONG_NAME as u32) != 0 {
            self.codec.write_long_name_len(writer, suffix_len)?;
        } else {
            writer.write_all(&[suffix_len as u8])?;
        }

        writer.write_all(&name[same_len..])
    }

    /// Writes symlink target if preserving links and entry is a symlink.
    ///
    /// Wire format: varint30(len) + raw bytes (no null terminator)
    ///
    /// // upstream: flist.c:send_file_entry() lines 660-670
    pub(super) fn write_symlink_target<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        if !self.preserve.links || !entry.is_symlink() {
            return Ok(());
        }

        if let Some(target) = entry.link_target() {
            // Symlink targets use the same wire-form normalisation as filenames:
            // any platform-native backslash separators are translated to forward
            // slashes before transmission.
            // upstream: flist.c:send_file_entry() lines 660-670 and util1.c:955-961
            let target_bytes = path_bytes_to_wire(target.as_path());
            // upstream: flist.c:1606-1621 - when sender_symlink_iconv (CF_SYMLINK_ICONV
            // negotiated) and a converter is configured, transcode the target through
            // ic_send (local -> wire / UTF-8) before writing.
            let target_bytes = self.apply_encoding_conversion(&target_bytes)?;
            let len = target_bytes.len();
            write_varint30_int(writer, len as i32, self.protocol.as_u8())?;
            writer.write_all(&target_bytes)?;
        }

        Ok(())
    }

    /// Writes device numbers if preserving devices and entry is a device.
    ///
    /// Also writes dummy rdev (0, 0) for special files (FIFOs, sockets) in protocol < 31.
    ///
    /// Wire format (protocol 28+):
    /// - Major: varint30 (omitted if XMIT_SAME_RDEV_MAJOR set)
    /// - Minor: varint (protocol 30+) or byte/int (protocol 28-29)
    ///
    /// // upstream: flist.c:send_file_entry() lines 640-660
    pub(super) fn write_rdev<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        let is_device = entry.is_device();
        let is_special = entry.is_special();

        // upstream: flist.c:send_file_entry() checks preserve_devices for
        // IS_DEVICE and preserve_specials for IS_SPECIAL separately
        let needs_rdev = (self.preserve.devices && is_device)
            || (self.preserve.specials && is_special && self.protocol.as_u8() < 31);

        if !needs_rdev {
            return Ok(());
        }

        let (major, minor) = if is_device {
            (
                entry.rdev_major().unwrap_or(0),
                entry.rdev_minor().unwrap_or(0),
            )
        } else {
            // Special file: dummy rdev (0, 0)
            (0, 0)
        };

        if xflags & ((XMIT_SAME_RDEV_MAJOR as u32) << 8) == 0 {
            write_varint30_int(writer, major as i32, self.protocol.as_u8())?;
        }

        if self.protocol.as_u8() >= 30 {
            write_varint(writer, minor as i32)?;
        } else {
            // Protocol 28-29: check XMIT_RDEV_MINOR_8_PRE30 flag
            let minor_8_bit = (xflags & ((XMIT_RDEV_MINOR_8_PRE30 as u32) << 8)) != 0;
            if minor_8_bit {
                writer.write_all(&[minor as u8])?;
            } else {
                writer.write_all(&(minor as i32).to_le_bytes())?;
            }
        }

        self.state.update_rdev_major(major);

        Ok(())
    }

    /// Writes hardlink index if preserving hardlinks and entry has one.
    ///
    /// Wire format (protocol 30+):
    /// - If XMIT_HLINKED is set but not XMIT_HLINK_FIRST: write varint index
    /// - If XMIT_HLINK_FIRST is also set: no index (this is the first/leader)
    ///
    /// // upstream: flist.c:send_file_entry() lines 583-595
    pub(super) fn write_hardlink_idx<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if !self.preserve.hard_links || self.protocol.as_u8() < 30 {
            return Ok(());
        }

        let hlinked = (xflags & ((XMIT_HLINKED as u32) << 8)) != 0;
        let hlink_first = (xflags & ((XMIT_HLINK_FIRST as u32) << 8)) != 0;

        if hlinked && !hlink_first {
            if let Some(idx) = entry.hardlink_idx() {
                write_varint(writer, idx as i32)?;
            }
        }

        Ok(())
    }

    /// Writes hardlink device and inode for protocol 28-29.
    ///
    /// In protocols before 30, hardlinks are identified by (dev, ino) pairs
    /// rather than indices. This writes the dev/ino after the symlink target.
    ///
    /// Wire format:
    /// - If not XMIT_SAME_DEV_PRE30: write longint(dev + 1)
    /// - Always write longint(ino)
    ///
    /// // upstream: flist.c:send_file_entry() lines 670-690
    pub(super) fn write_hardlink_dev_ino<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        // Only for protocol 28-29, non-directories with hardlink info
        if !self.preserve.hard_links
            || self.protocol.as_u8() >= 30
            || self.protocol.as_u8() < 28
            || entry.is_dir()
        {
            return Ok(());
        }

        let dev = match entry.hardlink_dev() {
            Some(d) => d,
            None => return Ok(()),
        };

        let ino = entry.hardlink_ino().unwrap_or(0);

        let same_dev = (xflags & ((XMIT_SAME_DEV_PRE30 as u32) << 8)) != 0;
        if !same_dev {
            // upstream: dev + 1 convention (0 reserved as sentinel)
            crate::write_longint(writer, dev + 1)?;
        }

        crate::write_longint(writer, ino)?;
        self.state.update_hardlink_dev(dev);

        Ok(())
    }

    /// Writes checksum if always_checksum mode is enabled.
    ///
    /// Wire format: raw bytes of length flist_csum_len
    /// - For regular files: actual checksum from entry
    /// - For non-regular files (proto < 28 only): empty_sum (all zeros)
    ///
    /// // upstream: flist.c:send_file_entry() lines 700-720
    pub(super) fn write_checksum<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
    ) -> io::Result<()> {
        if !self.always_checksum || self.flist_csum_len == 0 {
            return Ok(());
        }

        let is_regular = entry.is_file();

        // For protocol < 28, non-regular files also get a checksum (empty_sum)
        // For protocol >= 28, only regular files get checksums
        if !is_regular && self.protocol.as_u8() >= 28 {
            return Ok(());
        }

        // Max checksum length: SHA1 = 20 bytes. Stack array avoids per-entry heap allocation.
        const MAX_CSUM_LEN: usize = 20;
        let zeros = [0u8; MAX_CSUM_LEN];

        if is_regular {
            if let Some(sum) = entry.checksum() {
                let len = sum.len().min(self.flist_csum_len);
                writer.write_all(&sum[..len])?;
                // Pad with zeros if checksum is shorter than expected
                if len < self.flist_csum_len {
                    writer.write_all(&zeros[..self.flist_csum_len - len])?;
                }
            } else {
                // No checksum set, write zeros
                writer.write_all(&zeros[..self.flist_csum_len])?;
            }
        } else {
            // Non-regular file (proto < 28): write empty_sum (all zeros)
            writer.write_all(&zeros[..self.flist_csum_len])?;
        }

        Ok(())
    }

    /// Applies iconv encoding conversion to a filename.
    ///
    /// When `--iconv` is used, filenames are converted from the local encoding
    /// to the remote encoding before transmission.
    pub(super) fn apply_encoding_conversion<'a>(
        &self,
        name: &'a [u8],
    ) -> io::Result<std::borrow::Cow<'a, [u8]>> {
        if let Some(ref converter) = self.iconv {
            match converter.local_to_remote(name) {
                Ok(converted) => Ok(converted),
                Err(e) => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("filename encoding conversion failed: {e}"),
                )),
            }
        } else {
            Ok(std::borrow::Cow::Borrowed(name))
        }
    }

    /// Returns true if this entry is a hardlink follower (metadata should be skipped).
    ///
    /// A hardlink follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST.
    /// Such entries reference another entry in the file list, so their metadata
    /// (size, mtime, mode, uid, gid, symlink, rdev) is omitted from the wire.
    #[inline]
    pub(super) fn is_hardlink_follower(&self, xflags: u32) -> bool {
        let hlinked = (xflags & ((XMIT_HLINKED as u32) << 8)) != 0;
        let hlink_first = (xflags & ((XMIT_HLINK_FIRST as u32) << 8)) != 0;
        hlinked && !hlink_first
    }

    /// Writes the end-of-list marker to terminate the file list.
    ///
    /// Three encoding modes:
    /// - Varint mode: zero varint + error code varint
    /// - Safe file list with error: two-byte sentinel + varint error code
    /// - Normal: single zero byte
    ///
    /// // upstream: flist.c:send_file_list() end-of-list write
    pub fn write_end<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        io_error: Option<i32>,
    ) -> io::Result<()> {
        if self.use_varint_flags() {
            // Varint mode: zero flags + error code
            write_varint(writer, 0)?;
            write_varint(writer, io_error.unwrap_or(0))?;
            return Ok(());
        }

        if let Some(error) = io_error
            && self.use_safe_file_list()
        {
            // Error marker + code
            let marker_lo = XMIT_EXTENDED_FLAGS;
            let marker_hi = XMIT_IO_ERROR_ENDLIST;
            writer.write_all(&[marker_lo, marker_hi])?;
            write_varint(writer, error)?;
            return Ok(());
        }

        // Normal end marker
        writer.write_all(&[0u8])
    }

    /// Updates file list statistics based on the entry type.
    ///
    /// // upstream: flist.c:send_file_list() stat accumulation at end of loop
    pub(super) fn update_stats(&mut self, entry: &FileEntry) {
        if entry.is_dir() {
            self.stats.num_dirs += 1;
        } else if entry.is_file() {
            self.stats.num_files += 1;
            self.stats.total_size += entry.size();
        } else if entry.is_symlink() {
            self.stats.num_symlinks += 1;
            // Symlinks contribute their target length to total_size in rsync
            if let Some(target) = entry.link_target() {
                self.stats.total_size += target.as_os_str().len() as u64;
            }
        } else if entry.is_device() {
            self.stats.num_devices += 1;
        } else if entry.is_special() {
            self.stats.num_specials += 1;
        }
    }
}
