//! Metadata field encoding for file list entries.
//!
//! Writes size, time, mode, atime, uid/gid, and owner name fields
//! in the upstream rsync wire format order.
//!
//! # Upstream Reference
//!
//! See `flist.c:send_file_entry()` lines 580-640 for the metadata writing logic.

use std::io::{self, Write};

use crate::codec::ProtocolCodec;
use crate::varint::write_varint;

use super::super::entry::FileEntry;
use super::super::flags::{
    XMIT_CRTIME_EQ_MTIME, XMIT_GROUP_NAME_FOLLOWS, XMIT_MOD_NSEC, XMIT_SAME_ATIME, XMIT_SAME_GID,
    XMIT_SAME_MODE, XMIT_SAME_TIME, XMIT_SAME_UID, XMIT_USER_NAME_FOLLOWS,
};
use super::FileListWriter;

impl FileListWriter {
    /// Writes metadata fields in upstream rsync wire format order.
    ///
    /// Order (matching flist.c send_file_entry lines 580-620):
    /// 1. size (varlong30)
    /// 2. mtime (if not XMIT_SAME_TIME)
    /// 3. nsec (if XMIT_MOD_NSEC, protocol 31+)
    /// 4. crtime (if preserving, not XMIT_CRTIME_EQ_MTIME)
    /// 5. mode (if not XMIT_SAME_MODE)
    /// 6. atime (if preserving, non-dir, not XMIT_SAME_ATIME)
    /// 7. uid + user name (if preserving, not XMIT_SAME_UID)
    /// 8. gid + group name (if preserving, not XMIT_SAME_GID)
    pub(super) fn write_metadata<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        self.write_size(writer, entry)?;
        self.write_time_fields(writer, entry, xflags)?;
        self.write_mode(writer, entry, xflags)?;
        self.write_atime(writer, entry, xflags)?;
        self.write_uid_field(writer, entry, xflags)?;
        self.write_gid_field(writer, entry, xflags)?;
        Ok(())
    }

    /// Writes file size using protocol-appropriate encoding.
    #[inline]
    fn write_size<W: Write + ?Sized>(&self, writer: &mut W, entry: &FileEntry) -> io::Result<()> {
        self.codec.write_file_size(writer, entry.size() as i64)
    }

    /// Writes time-related fields: mtime, nsec, and crtime.
    #[inline]
    fn write_time_fields<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if xflags & (XMIT_SAME_TIME as u32) == 0 {
            self.codec.write_mtime(writer, entry.mtime())?;
        }

        if (xflags & ((XMIT_MOD_NSEC as u32) << 8)) != 0 {
            write_varint(writer, entry.mtime_nsec() as i32)?;
        }

        if self.preserve.crtimes && (xflags & ((XMIT_CRTIME_EQ_MTIME as u32) << 16)) == 0 {
            crate::write_varlong(writer, entry.crtime(), 4)?;
        }

        Ok(())
    }

    /// Writes mode field if different from previous entry.
    #[inline]
    fn write_mode<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if xflags & (XMIT_SAME_MODE as u32) == 0 {
            let wire_mode = super::super::wire_mode::to_wire_mode(entry.mode());
            writer.write_all(&wire_mode.to_le_bytes())?;
        }
        Ok(())
    }

    /// Writes atime field if preserving and different (non-directories only).
    ///
    /// For protocol >= 32, also writes atime nanoseconds as a varint
    /// after the atime seconds value.
    #[inline]
    fn write_atime<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        if self.preserve.atimes
            && !entry.is_dir()
            && (xflags & ((XMIT_SAME_ATIME as u32) << 8)) == 0
        {
            crate::write_varlong(writer, entry.atime(), 4)?;
            if self.protocol.as_u8() >= 32 {
                write_varint(writer, entry.atime_nsec() as i32)?;
            }
            self.state.update_atime(entry.atime());
        }
        Ok(())
    }

    /// Writes UID and optional user name if preserving and different.
    #[inline]
    fn write_uid_field<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        let entry_uid = entry.uid().unwrap_or(0);
        if !self.preserve.uid || (xflags & (XMIT_SAME_UID as u32)) != 0 {
            return Ok(());
        }

        if self.protocol.uses_fixed_encoding() {
            writer.write_all(&(entry_uid as i32).to_le_bytes())?;
        } else {
            write_varint(writer, entry_uid as i32)?;
            if (xflags & ((XMIT_USER_NAME_FOLLOWS as u32) << 8)) != 0 {
                self.write_owner_name(writer, entry.user_name())?;
            }
        }
        self.state.update_uid(entry_uid);
        Ok(())
    }

    /// Writes GID and optional group name if preserving and different.
    #[inline]
    fn write_gid_field<W: Write + ?Sized>(
        &mut self,
        writer: &mut W,
        entry: &FileEntry,
        xflags: u32,
    ) -> io::Result<()> {
        let entry_gid = entry.gid().unwrap_or(0);
        if !self.preserve.gid || (xflags & (XMIT_SAME_GID as u32)) != 0 {
            return Ok(());
        }

        if self.protocol.uses_fixed_encoding() {
            writer.write_all(&(entry_gid as i32).to_le_bytes())?;
        } else {
            write_varint(writer, entry_gid as i32)?;
            if (xflags & ((XMIT_GROUP_NAME_FOLLOWS as u32) << 8)) != 0 {
                self.write_owner_name(writer, entry.group_name())?;
            }
        }
        self.state.update_gid(entry_gid);
        Ok(())
    }

    /// Writes a user or group name (truncated to 255 bytes).
    ///
    /// The name is preceded by a single length byte, limiting names to 255
    /// bytes. Longer names are silently truncated.
    ///
    /// // upstream: flist.c:send_file_entry() - uid_ndx/gid_ndx name writing
    #[inline]
    fn write_owner_name<W: Write + ?Sized>(
        &self,
        writer: &mut W,
        name: Option<&str>,
    ) -> io::Result<()> {
        if let Some(name) = name {
            let name_bytes = name.as_bytes();
            let len = name_bytes.len().min(255) as u8;
            writer.write_all(&[len])?;
            writer.write_all(&name_bytes[..len as usize])?;
        }
        Ok(())
    }
}
