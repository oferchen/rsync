//! Extra field reading: symlinks, device numbers, hardlinks, checksums, and stats.
//!
//! These fields are conditionally present based on file type and protocol options,
//! and are read after the core metadata (name, size, mtime, mode, uid, gid).

use std::io::{self, Read};
use std::path::PathBuf;

use crate::flist::entry::FileEntry;
use crate::flist::flags::{FileFlags, XMIT_HLINK_FIRST, XMIT_HLINKED};
use crate::varint::{read_varint, read_varint30_int};

use super::FileListReader;

impl FileListReader {
    /// Reads symlink target if mode indicates a symlink AND preserve_links is enabled.
    ///
    /// The sender only transmits symlink targets when preserve_links is negotiated.
    /// If preserve_links is false, the sender omits symlink targets, so we must NOT
    /// attempt to read them from the stream.
    ///
    /// Wire format: varint30(len) + raw bytes
    ///
    /// upstream: flist.c:recv_file_entry() lines 920-935
    ///
    /// `name` is the entry's already-read wire filename, used only to render the
    /// `cannot convert symlink data for` diagnostic on a strict conversion
    /// failure (mirroring upstream's `full_fname(thisname)`).
    pub(super) fn read_symlink_target<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        mode: u32,
        name: &[u8],
    ) -> io::Result<Option<PathBuf>> {
        let is_symlink = mode & 0o170000 == 0o120000;

        if !is_symlink || !self.preserve_links {
            return Ok(None);
        }

        let len = read_varint30_int(reader, self.protocol.as_u8())? as usize;
        if len == 0 {
            return Ok(None);
        }

        // upstream: rsync.h MAXPATHLEN - reject targets that exceed PATH_MAX to
        // prevent unbounded allocation from a malicious sender.
        if len > crate::wire::file_entry_decode::MAX_SYMLINK_TARGET_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "symlink target length {len} exceeds maximum {}",
                    crate::wire::file_entry_decode::MAX_SYMLINK_TARGET_LEN
                ),
            ));
        }

        let mut target_bytes = vec![0u8; len];
        reader.read_exact(&mut target_bytes)?;

        // upstream: flist.c:1156 - the target is transcoded through ic_recv ONLY
        // when `sender_symlink_iconv` (iconv active AND CF_SYMLINK_ICONV
        // negotiated). Against a peer that lacks the capability the raw local
        // bytes are read verbatim (flist.c:1181 `read_sbuf` else branch), so a
        // proto-30 / pre-3.1 peer must NOT be transcoded here.
        let target_bytes: std::borrow::Cow<'_, [u8]> = if self.symlink_iconv {
            match self.iconv.as_ref() {
                Some(converter) => match converter.remote_to_local(&target_bytes) {
                    Ok(converted) => converted,
                    Err(_) => {
                        // upstream: flist.c:1169-1177 - strict `ic_recv` failure
                        // warns via FERROR_XFER, sets io_error |= IOERR_GENERAL,
                        // and empties the target (bp[0]='\0', outbuf.len=0). The
                        // entry stays a symlink with an empty target.
                        eprintln!(
                            "{}",
                            crate::iconv::cannot_convert_symlink_message("receiver", name)
                        );
                        self.io_error |= 1;
                        return Ok(Some(PathBuf::new()));
                    }
                },
                None => std::borrow::Cow::Borrowed(target_bytes.as_slice()),
            }
        } else {
            std::borrow::Cow::Borrowed(target_bytes.as_slice())
        };

        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let target = std::ffi::OsStr::from_bytes(&target_bytes);
            Ok(Some(PathBuf::from(target)))
        }
        #[cfg(not(unix))]
        {
            let target_str = String::from_utf8_lossy(&target_bytes);
            Ok(Some(PathBuf::from(target_str.into_owned())))
        }
    }

    /// Reads device numbers if preserving devices and mode indicates a device.
    ///
    /// Also reads dummy rdev for special files (FIFOs, sockets) in protocol < 31.
    ///
    /// Wire format (protocol 28+):
    /// - Major: varint30 (omitted if XMIT_SAME_RDEV_MAJOR set)
    /// - Minor: varint (protocol 30+) or byte/int (protocol 28-29)
    ///
    /// upstream: flist.c:recv_file_entry() lines 936-970
    pub(super) fn read_rdev<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        mode: u32,
        flags: FileFlags,
    ) -> io::Result<Option<(u32, u32)>> {
        let type_bits = mode & 0o170000;
        let is_device = type_bits == 0o060000 || type_bits == 0o020000; // S_ISBLK or S_ISCHR
        let is_special = type_bits == 0o140000 || type_bits == 0o010000; // S_IFSOCK or S_IFIFO

        // upstream: flist.c checks preserve_devices for IS_DEVICE and
        // preserve_specials for IS_SPECIAL separately
        let needs_rdev = (self.preserve_devices && is_device)
            || (self.preserve_specials && is_special && self.protocol.as_u8() < 31);

        if !needs_rdev {
            return Ok(None);
        }

        let major = if flags.same_rdev_major() {
            self.state.prev_rdev_major()
        } else {
            let m = read_varint30_int(reader, self.protocol.as_u8())? as u32;
            self.state.update_rdev_major(m);
            m
        };

        let minor = if self.protocol.as_u8() >= 30 {
            read_varint(reader)? as u32
        } else {
            // Protocol 28-29: read byte or int based on XMIT_RDEV_MINOR_8_pre30
            let minor_is_byte = flags.rdev_minor_8_pre30();
            if minor_is_byte {
                let mut buf = [0u8; 1];
                reader.read_exact(&mut buf)?;
                buf[0] as u32
            } else {
                let mut buf = [0u8; 4];
                reader.read_exact(&mut buf)?;
                i32::from_le_bytes(buf) as u32
            }
        };

        // For special files, we read but don't return the dummy rdev
        if is_special {
            return Ok(None);
        }

        Ok(Some((major, minor)))
    }

    /// Reads hardlink index if preserving hardlinks and flags indicate it.
    ///
    /// Wire format (protocol 30+):
    /// - If XMIT_HLINKED is set but not XMIT_HLINK_FIRST: read varint index
    /// - If XMIT_HLINK_FIRST is also set: return u32::MAX (this is the first/leader)
    ///
    /// upstream: flist.c:recv_file_entry() lines 800-815
    pub(super) fn read_hardlink_idx<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        flags: FileFlags,
    ) -> io::Result<Option<u32>> {
        if !self.preserve_hard_links || self.protocol.as_u8() < 30 {
            return Ok(None);
        }

        let hlinked = (flags.extended & XMIT_HLINKED) != 0;
        if !hlinked {
            return Ok(None);
        }

        let hlink_first = (flags.extended & XMIT_HLINK_FIRST) != 0;
        if hlink_first {
            return Ok(Some(u32::MAX));
        }

        let idx = read_varint(reader)? as u32;
        Ok(Some(idx))
    }

    /// Reads hardlink device and inode for protocol 28-29.
    ///
    /// In protocols before 30, hardlinks are identified by (dev, ino) pairs
    /// rather than indices. Only reads when XMIT_HLINKED is set - non-hardlinked
    /// entries have no dev/ino data on the wire.
    ///
    /// Wire format:
    /// - If not XMIT_SAME_DEV_PRE30: read longint as dev (stored as dev + 1)
    /// - Always read longint as ino
    ///
    /// upstream: flist.c:recv_file_entry() - dev/ino read is gated on
    /// `preserve_hard_links && xflags & XMIT_HLINKED`
    pub(super) fn read_hardlink_dev_ino<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        flags: FileFlags,
        mode: u32,
    ) -> io::Result<Option<(i64, i64)>> {
        if !self.preserve_hard_links || self.protocol.as_u8() >= 30 || self.protocol.as_u8() < 28 {
            return Ok(None);
        }

        // upstream: flist.c:recv_file_entry() only reads dev/ino when
        // XMIT_HLINKED is set. Non-hardlinked entries have no dev/ino on wire.
        if !flags.hlinked() {
            return Ok(None);
        }

        let is_dir = (mode & 0o170000) == 0o040000;
        if is_dir {
            return Ok(None);
        }

        let dev = if flags.same_dev_pre30() {
            self.state.prev_hardlink_dev()
        } else {
            let raw_dev = crate::read_longint(reader)?;
            // Upstream stores dev + 1, so subtract 1. upstream: flist.c:1177
            // `dev = read_longint(f)` then the +1 offset is undone with plain
            // int64 arithmetic that wraps; a malicious sender can supply
            // raw_dev == i64::MIN, so wrapping_sub matches upstream's wrap and
            // avoids an overflow-checks panic (cargo-fuzz / debug builds).
            let dev = raw_dev.wrapping_sub(1);
            self.state.update_hardlink_dev(dev);
            dev
        };

        let ino = crate::read_longint(reader)?;

        Ok(Some((dev, ino)))
    }

    /// Reads checksum if always_checksum mode is enabled.
    ///
    /// Wire format: raw bytes of length flist_csum_len
    ///
    /// upstream: flist.c:recv_file_entry() lines 1010-1030
    pub(super) fn read_checksum<R: Read + ?Sized>(
        &self,
        reader: &mut R,
        mode: u32,
    ) -> io::Result<Option<Vec<u8>>> {
        if !self.always_checksum || self.flist_csum_len == 0 {
            return Ok(None);
        }

        let is_regular = (mode & 0o170000) == 0o100000; // S_IFREG

        // For protocol < 28, non-regular files also have checksums (empty_sum)
        // For protocol >= 28, only regular files have checksums
        if !is_regular && self.protocol.as_u8() >= 28 {
            return Ok(None);
        }

        let mut checksum = vec![0u8; self.flist_csum_len];
        reader.read_exact(&mut checksum)?;

        // For non-regular files, the checksum is empty_sum (all zeros), don't store
        if !is_regular {
            return Ok(None);
        }

        Ok(Some(checksum))
    }

    /// Updates file list statistics based on the entry type.
    ///
    /// Tracks counts of files, directories, symlinks, devices, and special files,
    /// as well as total size for files and symlink targets.
    ///
    /// upstream: flist.c:recv_file_list() stat accumulation at end of loop
    pub(super) fn update_stats(&mut self, entry: &FileEntry) {
        if entry.is_dir() {
            self.stats.num_dirs += 1;
        } else if entry.is_file() {
            self.stats.num_files += 1;
            // upstream: flist.c:691 `stats.total_size += F_LENGTH(file)` uses
            // signed int64 and tolerates wrap; this is a cosmetic counter logged
            // by trace.rs and never gates wire format. Saturate instead of
            // wrapping so debug builds (overflow-checks=true, e.g. cargo-fuzz)
            // do not panic on adversarial sizes.
            self.stats.total_size = self.stats.total_size.saturating_add(entry.size());
        } else if entry.is_symlink() {
            self.stats.num_symlinks += 1;
            if let Some(target) = entry.link_target() {
                self.stats.total_size = self
                    .stats
                    .total_size
                    .saturating_add(target.as_os_str().len() as u64);
            }
        } else if entry.is_device() {
            self.stats.num_devices += 1;
        } else if entry.is_special() {
            self.stats.num_specials += 1;
        }
    }
}

#[cfg(test)]
mod edg_panic_tests {
    use crate::flist::flags::{FileFlags, XMIT_HLINKED};
    use crate::flist::read::FileListReader;
    use crate::version::ProtocolVersion;

    /// A malicious sender must not crash the protocol 28-29 hardlink decode by
    /// sending dev == i64::MIN, which would underflow the `raw_dev - 1` offset
    /// (upstream: flist.c:1177) and panic under overflow-checks (cargo-fuzz /
    /// debug). The hardened decode mirrors upstream's int64 wraparound and must
    /// return cleanly instead of panicking.
    #[test]
    fn read_hardlink_dev_ino_tolerates_min_dev_without_panic() {
        let proto = ProtocolVersion::from_supported(28).expect("protocol 28 supported");
        let mut reader = FileListReader::new(proto).with_preserve_hard_links(true);

        // dev: read_longint sentinel 0xFFFFFFFF then i64::MIN little-endian.
        // ino: a plain 4-byte longint (0).
        let mut wire = Vec::new();
        wire.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
        wire.extend_from_slice(&i64::MIN.to_le_bytes());
        wire.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        // XMIT_HLINKED set, XMIT_SAME_DEV_PRE30 clear -> dev read from the wire.
        let flags = FileFlags::new(0, XMIT_HLINKED);
        let mode = 0o100_644; // regular file, not a directory

        let (dev, ino) = reader
            .read_hardlink_dev_ino(&mut &wire[..], flags, mode)
            .expect("decode must not error")
            .expect("hardlink dev/ino present");
        assert_eq!(dev, i64::MAX); // i64::MIN.wrapping_sub(1)
        assert_eq!(ino, 0);
    }
}
