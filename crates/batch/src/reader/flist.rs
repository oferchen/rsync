//! File list deserialization for batch files.
//!
//! Provides methods for reading file list entries from batch files using
//! both the protocol wire format (upstream-compatible) and a local encoding.

use super::BatchReader;
use crate::error::{BatchError, BatchResult};
use crate::format::FileEntry;
use protocol::CompatibilityFlags;
use protocol::ProtocolVersion;
use protocol::codec::NdxCodecEnum;
use protocol::flist::FileListReader;
use protocol::idlist::IdList;
use std::io;

impl BatchReader {
    /// Read a file entry from the batch file using local encoding.
    ///
    /// Returns the next file list entry, or None if end of file list is reached.
    ///
    /// **Note:** This uses a local serialization format that is not compatible
    /// with upstream rsync's batch files. For protocol-compatible batch files,
    /// use [`read_protocol_flist`](Self::read_protocol_flist) instead.
    pub fn read_file_entry(&mut self) -> BatchResult<Option<FileEntry>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before file entries",
            )));
        }

        if let Some(ref mut reader) = self.batch_file {
            // EOF or an empty path marks the end of the file list.
            match FileEntry::read_from(reader) {
                Ok(entry) => {
                    if entry.path.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(entry))
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
                Err(e) => Err(BatchError::Io(io::Error::new(
                    e.kind(),
                    format!("Failed to read file entry: {e}"),
                ))),
            }
        } else {
            Err(BatchError::Io(io::Error::other("Batch file not open")))
        }
    }

    /// Read the entire file list from the batch file using the protocol flist
    /// decoder.
    ///
    /// This method decodes file list entries using the same wire format that
    /// upstream rsync uses in batch files - a raw tee of the protocol stream.
    /// The decoder is configured using the protocol version and compatibility
    /// flags from the batch header, plus the stream flags (preserve_uid, etc.)
    /// that were recorded when the batch was written.
    ///
    /// When INC_RECURSE is enabled (CF_INC_RECURSE in compat_flags), the batch
    /// stream contains multiple flist segments: an initial segment (just ".")
    /// followed by NDX-prefixed sub-list segments for each directory. This
    /// method reads all segments and returns a flat list of all entries.
    ///
    /// Returns all decoded file entries. After this call, the batch file reader
    /// is positioned at the start of the delta operations section. The NDX codec
    /// state is preserved for the delta replay phase.
    ///
    /// # Upstream Reference
    ///
    /// - `batch.c` - batch file body is a raw protocol stream tee
    /// - `flist.c:recv_file_entry()` - wire format decoded by `FileListReader`
    /// - `flist.c:recv_file_list()` - reads one flist segment
    /// - `flist.c:recv_additional_file_list()` - reads incremental sub-lists
    pub fn read_protocol_flist(&mut self) -> BatchResult<Vec<protocol::flist::FileEntry>> {
        if self.header.is_none() {
            return Err(BatchError::Io(io::Error::other(
                "Must read header before protocol flist",
            )));
        }

        let header = self.header.as_ref().expect("header checked above");
        let flags = header.stream_flags;

        let protocol_version =
            ProtocolVersion::try_from(header.protocol_version as u8).map_err(|_| {
                BatchError::InvalidFormat(format!(
                    "unsupported protocol version {} in batch header",
                    header.protocol_version,
                ))
            })?;

        let inc_recurse = header
            .compat_flags
            .map(|cf| {
                CompatibilityFlags::from_bits(cf as u32).contains(CompatibilityFlags::INC_RECURSE)
            })
            .unwrap_or(false);

        // Build the flist reader, configuring preserve flags to match the
        // options that were active when the batch was written.
        let mut flist_reader = if let Some(cf) = header.compat_flags {
            let compat = CompatibilityFlags::from_bits(cf as u32);
            FileListReader::with_compat_flags(protocol_version, compat)
        } else {
            FileListReader::new(protocol_version)
        };
        // upstream: batch.c flag_ptr[] - preserve_devices (bit 4) covers both
        // --devices and --specials (upstream `-D` = `--devices --specials`).
        // The flist reader needs both flags set to correctly decode device and
        // special file entries.
        flist_reader = flist_reader
            .with_preserve_uid(flags.preserve_uid)
            .with_preserve_gid(flags.preserve_gid)
            .with_preserve_links(flags.preserve_links)
            .with_preserve_devices(flags.preserve_devices)
            .with_preserve_specials(flags.preserve_devices)
            .with_preserve_hard_links(flags.preserve_hard_links)
            .with_preserve_acls(flags.preserve_acls)
            .with_preserve_xattrs(flags.preserve_xattrs);

        // upstream: flist.c:150 - when always_checksum is set, each regular file
        // entry in the flist carries a trailing checksum of flist_csum_len bytes.
        // Without this, the reader would skip those bytes and go out of sync.
        // The checksum length depends on the negotiated algorithm. For batch files
        // without explicit negotiation, the default is MD5 (protocol >= 30) or
        // MD4 (protocol < 30) - both produce 16-byte digests.
        if flags.always_checksum {
            let csum_len = default_flist_csum_len(header.protocol_version);
            flist_reader = flist_reader.with_always_checksum(csum_len);
        }

        let reader = self
            .batch_file
            .as_mut()
            .ok_or_else(|| BatchError::Io(io::Error::other("Batch file not open")))?;

        // Read the initial flist segment.
        let mut entries = Vec::new();
        loop {
            match flist_reader.read_entry_with_flist(reader, &entries) {
                Ok(Some(entry)) => entries.push(entry),
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    return Err(BatchError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed to read protocol flist entry: {e}"),
                    )));
                }
            }
        }

        // Capture any I/O error accumulated during flist reading.
        // upstream: flist.c:recv_file_list() does `io_error |= err` when the
        // sender reports errors, then breaks the loop without aborting.
        self.io_error = flist_reader.io_error();

        // upstream: flist.c:2726-2728 - recv_id_list(f, flist) when !inc_recurse
        // The batch stream contains uid/gid name mapping lists after the flist
        // entries. We must consume them to keep the stream position correct for
        // delta replay. Batch files don't record numeric_ids, but if the original
        // transfer used --numeric-ids, no ID lists were sent and none are in the
        // stream. Since the interop test default does NOT use --numeric-ids, we
        // consume them when preserve_uid/gid is set.
        if !inc_recurse {
            let id0_names = header
                .compat_flags
                .map(|cf| {
                    CompatibilityFlags::from_bits(cf as u32).contains(CompatibilityFlags::ID0_NAMES)
                })
                .unwrap_or(false);
            let proto_ver = protocol_version.as_u8();

            // upstream: uidlist.c:465 - (preserve_uid || preserve_acls) && numeric_ids <= 0
            if flags.preserve_uid || flags.preserve_acls {
                let mut uid_list = IdList::new();
                uid_list.read(reader, id0_names, proto_ver, |_| None)?;
            }

            // upstream: uidlist.c:473 - (preserve_gid || preserve_acls) && numeric_ids <= 0
            if flags.preserve_gid || flags.preserve_acls {
                let mut gid_list = IdList::new();
                gid_list.read(reader, id0_names, proto_ver, |_| None)?;
            }
        }

        // With INC_RECURSE, the batch stream interleaves flist sub-list
        // segments with delta operations (the batch file is a raw tee of the
        // protocol stream). We cannot read all sub-lists here because delta
        // NDX values appear between them. Instead, store the flist reader and
        // NDX codec so sub-lists can be read on-demand during delta replay.
        // upstream: main.c:do_recv() interleaves recv_additional_file_list()
        // with recv_files() in an event loop.
        if inc_recurse {
            self.ndx_codec = Some(NdxCodecEnum::new(protocol_version.as_u8()));
            // upstream: flist.c:2931 - ndx_start = prev->ndx_start + prev->used + 1
            // The initial flist has ndx_start=1, so the next sub-list starts at
            // 1 + entries.len() + 1 (the +1 gap between segments).
            self.flist_next_ndx_start = 1 + entries.len() as i32 + 1;
            self.flist_reader = Some(flist_reader);
        }

        Ok(entries)
    }

    /// Read one incremental flist sub-list segment from the batch stream.
    ///
    /// Called during delta replay when an NDX_FLIST_OFFSET value is encountered.
    /// Reads entries for one directory's sub-list and appends them to `entries`.
    /// The wire format already encodes full relative paths (dirname + basename)
    /// for each entry, so no parent directory prefix is needed.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:recv_additional_file_list()` - reads one sub-list segment
    pub fn read_incremental_flist_segment(
        &mut self,
        entries: &mut Vec<protocol::flist::FileEntry>,
    ) -> BatchResult<()> {
        let flist_reader = self
            .flist_reader
            .as_mut()
            .ok_or_else(|| BatchError::Io(io::Error::other("no flist reader for INC_RECURSE")))?;

        let reader = self
            .batch_file
            .as_mut()
            .ok_or_else(|| BatchError::Io(io::Error::other("Batch file not open")))?;

        // Reset compression state for the new segment.
        // upstream: recv_file_list() starts with fresh static state.
        flist_reader.reset_for_new_segment(self.flist_next_ndx_start);

        // Read entries for this sub-list segment.
        // upstream: flist.c:recv_file_entry() encodes dirname as part of each
        // entry's wire format. Sub-list entries already have full relative paths
        // (e.g., "subdir/f2.dat"), so no prepend_dir() is needed.
        let segment_start = entries.len();
        loop {
            let segment_entries = &entries[segment_start..];
            match flist_reader.read_entry_with_flist(reader, segment_entries) {
                Ok(Some(entry)) => {
                    entries.push(entry);
                }
                Ok(None) => break,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    return Err(BatchError::Io(io::Error::new(
                        e.kind(),
                        format!("Failed to read incremental flist entry: {e}"),
                    )));
                }
            }
        }

        self.io_error |= flist_reader.io_error();
        // upstream: flist.c:2931 - ndx_start = prev->ndx_start + prev->used + 1
        // The current segment started at flist_next_ndx_start (set before
        // reset_for_new_segment). The next segment's ndx_start accounts for
        // the entries in this segment plus the +1 gap.
        let segment_count = entries.len() - segment_start;
        self.flist_next_ndx_start += segment_count as i32 + 1;

        Ok(())
    }
}

/// Returns the default flist checksum length for a batch file.
///
/// Upstream `flist.c:150` computes `flist_csum_len = csum_len_for_type(file_sum_nni->num, 1)`.
/// Without explicit checksum negotiation (which batch files bypass), the default
/// file checksum algorithm is MD5 (protocol >= 30) or MD4 (protocol < 30). Both
/// produce 16-byte digests. Protocol < 27 with `CSUM_MD4_ARCHAIC` uses 2 bytes
/// for flist checksums, but we only support protocol >= 27.
///
/// # Upstream Reference
///
/// - `checksum.c:csum_len_for_type()` - MD4=16, MD5=16, XXH3_128=16, XXH64=8
pub(crate) fn default_flist_csum_len(protocol_version: i32) -> usize {
    // All supported protocols (27-32) default to MD4 or MD5, both 16 bytes.
    // If XXH3-128 is negotiated via checksum seeds, it is also 16 bytes.
    // XXH64 and XXH3-64 are 8 bytes but require explicit negotiation which
    // is not recorded in the batch stream flags. Conservative default: 16.
    let _ = protocol_version;
    16
}
