//! Metadata field reading from the rsync wire format.
//!
//! Handles decoding of file size, modification time, nanoseconds, creation time,
//! mode, access time, UID/GID, and associated owner/group names.
//!
//! # Upstream Reference
//!
//! See `flist.c:recv_file_entry()` lines 826-920 for the metadata reading logic.

use std::io::{self, Read};

use logging::debug_log;

use crate::codec::ProtocolCodec;
use crate::flist::flags::{FileFlags, XMIT_NO_CONTENT_DIR};
use crate::varint::read_varint;

use super::FileListReader;

/// Maximum wire-encoded nanosecond value accepted for `modtime_nsec`.
///
/// upstream: rsync.h `#define MAX_WIRE_NSEC 999999999` - the inclusive upper
/// bound `recv_file_entry()` passes to `read_varint_bounded()` (flist.c:855/857)
/// when decoding the sub-second modification time. A wire value outside
/// `[0, MAX_WIRE_NSEC]` is a protocol violation (`RERR_PROTOCOL`, exit 2).
const MAX_WIRE_NSEC: i32 = 999_999_999;

/// Decoded metadata fields for a single file entry.
///
/// Fields are `Option` when conditionally present based on protocol options
/// (preserve_uid, preserve_gid, preserve_atimes, etc.).
pub(crate) struct MetadataResult {
    /// Modification time in seconds since Unix epoch.
    pub mtime: i64,
    /// Nanosecond component of modification time (protocol 31+).
    pub nsec: u32,
    /// Unix mode bits (file type and permissions).
    pub mode: u32,
    /// User ID (when preserve_uid is enabled).
    pub uid: Option<u32>,
    /// Group ID (when preserve_gid is enabled).
    pub gid: Option<u32>,
    /// User name for UID mapping (protocol 30+).
    pub user_name: Option<String>,
    /// Group name for GID mapping (protocol 30+).
    pub group_name: Option<String>,
    /// Access time (when preserve_atimes is enabled, non-directories only).
    pub atime: Option<i64>,
    /// Nanosecond component of access time (protocol 32+, --atimes).
    pub atime_nsec: u32,
    /// Creation time (when preserve_crtimes is enabled).
    pub crtime: Option<i64>,
    /// Whether directory has content to transfer (protocol 30+, directories only).
    pub content_dir: bool,
}

impl FileListReader {
    /// Reads metadata fields in upstream rsync wire format order.
    ///
    /// Fields are read in this exact order (matching flist.c recv_file_entry lines 826-920):
    ///
    /// | Order | Field | Condition | Encoding |
    /// |-------|-------|-----------|----------|
    /// | 1 | mtime | `!XMIT_SAME_TIME` | varlong(4) |
    /// | 2 | nsec | `XMIT_MOD_NSEC` (proto 31+) | varint30 |
    /// | 3 | crtime | `preserve_crtimes && !XMIT_CRTIME_EQ_MTIME` | varlong(4) |
    /// | 4 | mode | `!XMIT_SAME_MODE` | i32 LE (proto <30) or varint |
    /// | 5 | atime | `preserve_atimes && !is_dir && !XMIT_SAME_ATIME` | varlong(4) |
    /// | 6 | uid | `preserve_uid && !XMIT_SAME_UID` | i32 LE (proto <30) or varint |
    /// | 6a | user_name | `XMIT_USER_NAME_FOLLOWS` (proto 30+) | u8 len + bytes |
    /// | 7 | gid | `preserve_gid && !XMIT_SAME_GID` | i32 LE (proto <30) or varint |
    /// | 7a | group_name | `XMIT_GROUP_NAME_FOLLOWS` (proto 30+) | u8 len + bytes |
    pub(super) fn read_metadata<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<MetadataResult> {
        // 1. Read mtime
        // upstream: flist.c:828-839 - proto >= 30 uses read_varlong(f, 4),
        // proto < 30 uses read_uint(f) (fixed 4-byte unsigned)
        let mtime = if flags.same_time() {
            self.state.prev_mtime()
        } else {
            let mtime = self.codec.read_mtime(reader)?;
            self.state.update_mtime(mtime);
            mtime
        };

        // 2. Read nanoseconds if flag set (protocol 31+)
        // upstream: flist.c:855/857 recv_file_entry() reads modtime_nsec via
        // read_varint_bounded(f, 0, MAX_WIRE_NSEC, "modtime_nsec")
        // (io.c:1904-1913), which aborts with exit_cleanup(RERR_PROTOCOL)
        // (exit 2) on a value outside [0, MAX_WIRE_NSEC]. Mirror that bound so a
        // hostile nsec yields RERR_PROTOCOL rather than being accepted unchecked.
        let nsec = if flags.mod_nsec() {
            let raw = read_varint(reader)?;
            if !(0..=MAX_WIRE_NSEC).contains(&raw) {
                return Err(crate::protocol_violation::protocol_violation(format!(
                    "modtime_nsec {raw} out of range: not in [0,{MAX_WIRE_NSEC}]"
                )));
            }
            raw as u32
        } else {
            0
        };

        // 3. Read crtime if preserving crtimes (BEFORE mode, per upstream)
        let crtime = if self.preserve_crtimes {
            if flags.crtime_eq_mtime() {
                Some(mtime)
            } else {
                let crtime = crate::read_varlong(reader, 4)?;
                Some(crtime)
            }
        } else {
            None
        };

        // 4. Read mode
        let mode = if flags.same_mode() {
            self.state.prev_mode()
        } else {
            let mut mode_bytes = [0u8; 4];
            reader.read_exact(&mut mode_bytes)?;
            let mode = super::super::wire_mode::from_wire_mode(i32::from_le_bytes(mode_bytes));
            self.state.update_mode(mode);
            mode
        };
        // upstream: flist.c:876-892 recv_file_entry() - reject any mode whose
        // S_IFMT type bits are not one of the standard file types, so a garbage
        // or malicious mode cannot propagate into the downstream S_ISxxx checks.
        // Upstream validates the FINAL resolved mode, whether freshly read or
        // inherited via XMIT_SAME_MODE, so the check lives outside the branch.
        // mode 0 is the sole exception, and only under --delete-missing-args
        // (missing_args == 2), the mode-0 sentinel for a vanished arg (flist.c:2257).
        if !(mode == 0 && self.delete_missing_args)
            && crate::flist::FileType::from_mode(mode).is_none()
        {
            // upstream: flist.c:890 exit_cleanup(RERR_PROTOCOL) (exit 2). Tag the
            // error so the core exit-code mapper yields RERR_PROTOCOL, not
            // RERR_STREAMIO(12).
            return Err(crate::protocol_violation::protocol_violation(format!(
                "invalid file mode 0{mode:o}"
            )));
        }

        // Determine if this is a directory (needed for atime and content_dir)
        let is_dir = (mode & 0o170000) == 0o040000;

        // 5. Read atime if preserving atimes (AFTER mode, non-directories only).
        // upstream: flist.c:894-895 - atime is a single `read_varlong(f, 4)`;
        // there is no atime nsec field on the wire regardless of protocol
        // version (unlike mtime nsec which is gated by XMIT_MOD_NSEC).
        let (atime, atime_nsec) = if self.preserve_atimes && !is_dir {
            if flags.same_atime() {
                (Some(self.state.prev_atime()), 0)
            } else {
                let atime = crate::read_varlong(reader, 4)?;
                self.state.update_atime(atime);
                (Some(atime), 0)
            }
        } else {
            (None, 0)
        };

        // 6. Read UID and optional user name
        // upstream: flist.c:880-890 - XMIT_USER_NAME_FOLLOWS only exists in
        // protocol >= 30. In protocol 28-29 that bit position is
        // XMIT_SAME_DEV_pre30, so we must not interpret it as name_follows.
        let uid_name_follows = self.protocol.as_u8() >= 30 && flags.user_name_follows();
        let (uid, user_name) = if self.preserve_uid {
            let (id, name) = read_owner_id(
                reader,
                flags.same_uid(),
                uid_name_follows,
                self.state.prev_uid(),
                self.protocol.uses_fixed_encoding(),
            )?;
            self.state.update_uid(id);
            (Some(id), name)
        } else {
            (None, None)
        };

        // 7. Read GID and optional group name
        // upstream: flist.c:891-902 - XMIT_GROUP_NAME_FOLLOWS only exists in
        // protocol >= 30. In protocol 28-29 that bit position is
        // XMIT_RDEV_MINOR_8_pre30.
        let gid_name_follows = self.protocol.as_u8() >= 30 && flags.group_name_follows();
        let (gid, group_name) = if self.preserve_gid {
            let (id, name) = read_owner_id(
                reader,
                flags.same_gid(),
                gid_name_follows,
                self.state.prev_gid(),
                self.protocol.uses_fixed_encoding(),
            )?;
            self.state.update_gid(id);
            (Some(id), name)
        } else {
            (None, None)
        };

        // Determine content_dir for directories (protocol 30+)
        // XMIT_NO_CONTENT_DIR shares bit with XMIT_SAME_RDEV_MAJOR but only applies to directories
        let content_dir = if is_dir && self.protocol.as_u8() >= 30 {
            (flags.extended & XMIT_NO_CONTENT_DIR) == 0
        } else {
            true
        };

        debug_log!(
            Flist,
            3,
            "read_metadata: mtime={} nsec={} mode={:o} uid={:?} gid={:?}",
            mtime,
            nsec,
            mode,
            uid,
            gid
        );

        Ok(MetadataResult {
            mtime,
            nsec,
            mode,
            uid,
            gid,
            user_name,
            group_name,
            atime,
            atime_nsec,
            crtime,
            content_dir,
        })
    }

    /// Reads the file size using protocol-appropriate encoding.
    ///
    /// The encoding varies by protocol version:
    /// - Protocol < 30: Fixed 32-bit or 64-bit encoding
    /// - Protocol 30+: Variable-length encoding (varlong30)
    pub(super) fn read_size<R: Read + ?Sized>(&self, reader: &mut R) -> io::Result<u64> {
        let size = self.codec.read_file_size(reader)?;
        debug_log!(Flist, 4, "read_size: size={}", size);
        Ok(size as u64)
    }
}

/// Reads an owner ID (uid or gid) and optional name from the wire.
///
/// Returns `(id, optional_name)`. When `same` is true, returns the previous
/// value unchanged. Otherwise reads the ID using fixed or varint encoding,
/// and optionally reads a name string if `name_follows` is set.
///
/// upstream: flist.c:recv_file_entry() lines 880-910 - uid/gid reading
fn read_owner_id<R: Read + ?Sized>(
    reader: &mut R,
    same: bool,
    name_follows: bool,
    prev_id: u32,
    fixed_encoding: bool,
) -> io::Result<(u32, Option<String>)> {
    if same {
        return Ok((prev_id, None));
    }

    let id = if fixed_encoding {
        let mut buf = [0u8; 4];
        reader.read_exact(&mut buf)?;
        i32::from_le_bytes(buf) as u32
    } else {
        read_varint(reader)? as u32
    };

    let name = if name_follows {
        let mut len_buf = [0u8; 1];
        reader.read_exact(&mut len_buf)?;
        let len = len_buf[0] as usize;
        if len > 0 {
            let mut name_bytes = vec![0u8; len];
            reader.read_exact(&mut name_bytes)?;
            Some(match String::from_utf8(name_bytes) {
                Ok(s) => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            })
        } else {
            None
        }
    } else {
        None
    };

    Ok((id, name))
}

#[cfg(test)]
mod nsec_tests {
    use std::io::Cursor;

    use crate::ProtocolVersion;
    use crate::flist::flags::{FileFlags, XMIT_MOD_NSEC, XMIT_SAME_TIME};
    use crate::varint::write_varint;

    use super::{FileListReader, MAX_WIRE_NSEC};

    fn reader() -> FileListReader {
        FileListReader::new(ProtocolVersion::try_from(31u8).unwrap())
    }

    // SAME_TIME reuses the previous second (no mtime field on the wire) and
    // MOD_NSEC signals the sub-second varint is present - the minimal flag set
    // that drives read_metadata straight to the nsec read.
    fn nsec_flags() -> FileFlags {
        FileFlags::new(XMIT_SAME_TIME, XMIT_MOD_NSEC)
    }

    #[test]
    fn modtime_nsec_in_range_still_parses() {
        // WHY: the new bound must not over-reject legitimate sub-second mtimes.
        // A value at exactly MAX_WIRE_NSEC is the largest upstream accepts
        // (flist.c:855/857 read_varint_bounded(f, 0, MAX_WIRE_NSEC, ...)).
        let mut buf = Vec::new();
        write_varint(&mut buf, MAX_WIRE_NSEC).unwrap();
        // read_metadata continues to the 4-byte mode after nsec; supply a
        // regular-file mode so the entry decodes cleanly.
        buf.extend_from_slice(&0o100644i32.to_le_bytes());

        let mut cursor = Cursor::new(buf);
        let meta = reader()
            .read_metadata(&mut cursor, nsec_flags())
            .expect("in-range nsec must parse");
        assert_eq!(meta.nsec, MAX_WIRE_NSEC as u32);
    }

    #[test]
    fn modtime_nsec_out_of_range_is_protocol_violation() {
        // WHY: upstream flist.c:855/857 bounds modtime_nsec to [0, MAX_WIRE_NSEC]
        // via read_varint_bounded (io.c:1904-1913), which
        // exit_cleanup(RERR_PROTOCOL) (exit 2) on an out-of-range value. A
        // drop-in tool must exit 2 (protocol incompatibility) on a hostile nsec,
        // never accept it unchecked nor exit RERR_STREAMIO (12); the
        // ProtocolViolation tag is what makes the core mapper reproduce exit 2.
        for raw in [MAX_WIRE_NSEC + 1, -1] {
            let mut buf = Vec::new();
            write_varint(&mut buf, raw).unwrap();
            let mut cursor = Cursor::new(buf);
            // MetadataResult is not Debug, so avoid expect_err/unwrap_err.
            let Err(err) = reader().read_metadata(&mut cursor, nsec_flags()) else {
                panic!("out-of-range nsec must be rejected");
            };
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert!(
                err.get_ref()
                    .is_some_and(|e| e.is::<crate::protocol_violation::ProtocolViolation>()),
                "out-of-range modtime_nsec must be tagged ProtocolViolation (RERR_PROTOCOL=2)"
            );
        }
    }
}
