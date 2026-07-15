//! Lazy, on-demand INC_RECURSE flist-segment fetch for the receiver.
//!
//! Upstream's generator fetches sub-list segments on demand: it reads one frame
//! at a time off the sender's stream (`io.c:read_ndx_and_attrs`), appending a
//! new segment whenever a `NDX_FLIST_OFFSET` marker arrives and stopping at
//! `NDX_FLIST_EOF`. The receiver never drains the whole list up front; it pulls
//! the next segment only when it needs an index the current list does not yet
//! cover. This module provides that primitive as methods on
//! [`ReceiverContext`]:
//!
//! - [`ReceiverContext::read_next_frame`] classifies and dispatches one frame,
//!   appending a segment via
//!   [`receive_one_extra_segment`](ReceiverContext::receive_one_extra_segment)
//!   and setting `flist_eof` at the terminator.
//! - [`ReceiverContext::ensure_flat_idx`] pulls segments until a target flat
//!   index is materialized (or the list ends), never indexing out of bounds.
//! - [`ReceiverContext::ensure_all_segments_loaded`] drains every remaining
//!   segment, reproducing the old up-front behaviour for the batched drivers.
//! - [`ReceiverContext::prefetch_for_hardlinks`] pre-reads segments so a
//!   follower's leader in a later segment is resolved before hardlinking.
//!
//! When INC_RECURSE is not negotiated, `flist_eof` is already set once
//! `receive_file_list` returns, so every method here is an immediate no-op that
//! performs no wire read - the transfer behaves exactly as before.
//!
//! # Upstream Reference
//!
//! - `rsync.c:318-429` - `read_ndx_and_attrs()` frame dispatch
//! - `io.c:1750-1786` - `wait_for_receiver()` one-frame fetch
//! - `generator.c:2299-2368` - `generate_files()` on-demand fetch loop

use std::io::{self, Read};

use protocol::codec::{NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum};

use super::super::ReceiverContext;

/// Classification of one frame read off the sender's flist/transfer stream.
///
/// Mirrors the four outcomes of upstream `read_ndx_and_attrs()`
/// (`rsync.c:318-429`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::receiver) enum FrameKind {
    /// An INC_RECURSE sub-list segment for the content-directory wire NDX was
    /// consumed and appended to `file_list`.
    Segment(i32),
    /// `NDX_FLIST_EOF`: no more segments follow. `flist_eof` is now set.
    FlistEof,
    /// `NDX_DONE`: the sender signalled phase completion.
    Done,
    /// A non-negative NDX - a per-file reply/echo.
    Reply(i32),
}

impl ReceiverContext {
    /// Reads and dispatches one frame off the sender's stream.
    ///
    /// A segment marker (`ndx <= NDX_FLIST_OFFSET`) is consumed in full via
    /// [`receive_one_extra_segment`](Self::receive_one_extra_segment) and
    /// reported as [`FrameKind::Segment`]. `NDX_FLIST_EOF` sets `flist_eof` and
    /// reports [`FrameKind::FlistEof`]; `NDX_DONE` reports [`FrameKind::Done`];
    /// any non-negative value reports [`FrameKind::Reply`].
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:330-381` - segment marker vs NDX_DONE vs positive-ndx dispatch
    pub(in crate::receiver) fn read_next_frame<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<FrameKind> {
        let ndx = ndx_codec.read_ndx(reader)?;

        if ndx == NDX_FLIST_EOF {
            self.flist_eof = true;
            return Ok(FrameKind::FlistEof);
        }
        if ndx == NDX_DONE {
            return Ok(FrameKind::Done);
        }
        if ndx <= NDX_FLIST_OFFSET {
            self.receive_one_extra_segment(reader, ndx)?;
            return Ok(FrameKind::Segment(NDX_FLIST_OFFSET - ndx));
        }
        Ok(FrameKind::Reply(ndx))
    }

    /// Ensures `file_list[flat_idx]` is materialized, pulling INC_RECURSE
    /// segments as needed.
    ///
    /// Returns `true` when an entry exists at `flat_idx` (the caller may index
    /// it), or `false` once the list is complete (`flist_eof`) and `flat_idx`
    /// is past the end. Never indexes out of bounds and never reads the wire
    /// when `flist_eof` is already set - so on a non-INC_RECURSE transfer this
    /// simply reports `flat_idx < file_list.len()` without touching the reader.
    ///
    /// Encountering a per-file [`FrameKind::Reply`] while fetching a segment is
    /// a protocol desync and surfaces as [`io::ErrorKind::InvalidData`].
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2299-2368` - fetch the next segment when the cursor
    ///   reaches the end of the current one and `!flist_eof`.
    pub(in crate::receiver) fn ensure_flat_idx<R: Read + ?Sized>(
        &mut self,
        flat_idx: usize,
        reader: &mut R,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<bool> {
        loop {
            if flat_idx < self.file_list.len() {
                return Ok(true);
            }
            if self.flist_eof {
                return Ok(false);
            }
            match self.read_next_frame(reader, ndx_codec)? {
                FrameKind::Segment(_) | FrameKind::FlistEof => {}
                FrameKind::Done => {
                    // The sender signalled completion before NDX_FLIST_EOF;
                    // treat it as the end of the list.
                    self.flist_eof = true;
                    return Ok(false);
                }
                FrameKind::Reply(ndx) => return Err(unexpected_reply(ndx)),
            }
        }
    }

    /// Drains every remaining INC_RECURSE segment until `flist_eof`.
    ///
    /// Reproduces the pre-refactor behaviour where the whole list was
    /// materialized up front, so the batched pipelined drivers see a complete
    /// `file_list`. A no-op (no wire read) once `flist_eof` is set, which is
    /// always the case on a non-INC_RECURSE transfer by the time a driver calls
    /// this.
    pub(in crate::receiver) fn ensure_all_segments_loaded<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
        while !self.flist_eof {
            match self.read_next_frame(reader, ndx_codec)? {
                FrameKind::Segment(_) | FrameKind::FlistEof => {}
                FrameKind::Done => self.flist_eof = true,
                FrameKind::Reply(ndx) => return Err(unexpected_reply(ndx)),
            }
        }
        Ok(())
    }

    /// Pre-reads segments until the list holds `hardlink_lookahead_target`
    /// entries (or `flist_eof`), so a follower whose leader arrives in a later
    /// segment is resolved before hardlinking.
    ///
    /// A no-op when `flist_eof` is already set (every non-INC_RECURSE transfer).
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:2300-2305` - `preserve_hard_links && inc_recurse`
    ///   pre-reads until `file_total < MIN_FILECNT_LOOKAHEAD / 2`.
    pub(in crate::receiver) fn prefetch_for_hardlinks<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
        while !self.flist_eof && self.file_list.len() < self.hardlink_lookahead_target {
            match self.read_next_frame(reader, ndx_codec)? {
                FrameKind::Segment(_) | FrameKind::FlistEof => {}
                FrameKind::Done => self.flist_eof = true,
                // A per-file reply here means the transfer phase has started;
                // stop prefetching rather than treat it as a desync.
                FrameKind::Reply(_) => break,
            }
        }
        Ok(())
    }
}

/// Builds the "unexpected per-file NDX while fetching a segment" error.
fn unexpected_reply(ndx: i32) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "unexpected per-file NDX {ndx} while fetching file-list segment {}{}",
            crate::role_trailer::error_location!(),
            crate::role_trailer::receiver()
        ),
    )
}

#[cfg(test)]
mod tests {
    //! Lazy segment-fetch against a mock throttled sender.
    //!
    //! Builds a `Cursor<Vec<u8>>` wire the same way the receiver's own
    //! wire-parity tests do (`FileListWriter` for the segment entries, the NDX
    //! codec for the `NDX_FLIST_OFFSET` / `NDX_FLIST_EOF` framing), simulating a
    //! sender that pushes several sub-list segments before the terminator. The
    //! test proves `ensure_flat_idx` grows `file_list` one segment at a time and
    //! reports `flist_eof` exactly at the marker - never over-reading and never
    //! indexing out of bounds.

    use std::ffi::OsString;
    use std::io::Cursor;
    use std::path::PathBuf;

    use protocol::codec::{NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, create_ndx_codec};
    use protocol::flist::{FileEntry, FileListWriter};
    use protocol::{CompatibilityFlags, ProtocolVersion};

    use crate::config::ServerConfig;
    use crate::handshake::HandshakeResult;
    use crate::receiver::ReceiverContext;
    use crate::role::ServerRole;

    const PROTOCOL: u8 = 32;

    /// Protocol-32 receiver config with no flags set (mirrors the shared
    /// `test_config` fixture, inlined because that helper is `pub(super)` to the
    /// receiver test tree).
    fn test_config() -> ServerConfig {
        ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
            flag_string: "-logDtpre.".to_owned(),
            args: vec![OsString::from(".")],
            ..Default::default()
        }
    }

    /// Protocol-32 handshake with no compat flags.
    fn test_handshake() -> HandshakeResult {
        HandshakeResult {
            protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        }
    }

    /// Encodes `segments.len()` INC_RECURSE sub-list segments (each with its own
    /// `NDX_FLIST_OFFSET - dir_ndx` marker and entries) followed by
    /// `NDX_FLIST_EOF`. Returns the wire bytes and the total entry count.
    fn encode_segments(segments: &[Vec<(&str, u64)>]) -> (Vec<u8>, usize) {
        let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
        let mut writer = FileListWriter::new(protocol);
        let mut ndx_codec = create_ndx_codec(PROTOCOL);
        let mut wire = Vec::new();
        let mut total = 0;

        for (dir_ndx, entries) in segments.iter().enumerate() {
            ndx_codec
                .write_ndx(&mut wire, NDX_FLIST_OFFSET - dir_ndx as i32)
                .unwrap();
            for (name, size) in entries {
                let mut e = FileEntry::new_file(PathBuf::from(name), *size, 0o100644);
                e.set_mtime(1_700_000_000, 0);
                writer.write_entry(&mut wire, &e).unwrap();
                total += 1;
            }
            writer.write_end(&mut wire, None).unwrap();
        }
        ndx_codec.write_ndx(&mut wire, NDX_FLIST_EOF).unwrap();
        (wire, total)
    }

    fn inc_recurse_receiver() -> ReceiverContext {
        let mut handshake = test_handshake();
        handshake.compat_flags = Some(CompatibilityFlags::INC_RECURSE);
        ReceiverContext::new_for_test(&handshake, test_config())
    }

    #[test]
    fn ensure_flat_idx_pulls_segments_lazily_until_eof() {
        // Three segments of 2, 3, and 1 entries: a sender that interleaves
        // several pushes before throttling at the terminator.
        let segments = vec![
            vec![("dir0/a.txt", 10u64), ("dir0/b.txt", 20)],
            vec![("dir1/c.txt", 30), ("dir1/d.txt", 40), ("dir1/e.txt", 50)],
            vec![("dir2/f.txt", 60)],
        ];
        let (wire, total) = encode_segments(&segments);
        assert_eq!(total, 6);

        let mut ctx = inc_recurse_receiver();
        // Stand in for an initial flist that already carried the three parent
        // directories (dir0..dir2), so each sub-list's dir_ndx (0..2) passes the
        // fail-closed `dir_ndx >= dir_flist_used` range check.
        ctx.dir_flist_used = segments.len();
        // A fresh INC_RECURSE receiver has no entries yet and is not at EOF.
        assert_eq!(ctx.file_list().len(), 0);
        assert!(!ctx.flist_eof);

        let mut reader = Cursor::new(wire);
        let mut codec = create_ndx_codec(PROTOCOL);

        // Index 0 pulls the first segment (2 entries), so the list grows to 2.
        assert!(ctx.ensure_flat_idx(0, &mut reader, &mut codec).unwrap());
        assert_eq!(ctx.file_list().len(), 2);
        assert!(!ctx.flist_eof);

        // Index 1 is already covered - no further read.
        let pos_before = reader.position();
        assert!(ctx.ensure_flat_idx(1, &mut reader, &mut codec).unwrap());
        assert_eq!(ctx.file_list().len(), 2);
        assert_eq!(
            reader.position(),
            pos_before,
            "index within segment re-read the wire"
        );

        // Index 2 pulls the second segment (3 entries) -> 5.
        assert!(ctx.ensure_flat_idx(2, &mut reader, &mut codec).unwrap());
        assert_eq!(ctx.file_list().len(), 5);

        // Walk to the last real entry, pulling the third segment (1 entry) -> 6.
        assert!(ctx.ensure_flat_idx(5, &mut reader, &mut codec).unwrap());
        assert_eq!(ctx.file_list().len(), total);

        // One past the end reads the NDX_FLIST_EOF marker and reports no entry.
        assert!(!ctx.ensure_flat_idx(6, &mut reader, &mut codec).unwrap());
        assert!(
            ctx.flist_eof,
            "flist_eof must be set once the terminator is read"
        );

        // Idempotent past EOF: no more reads, still no entry.
        let pos_eof = reader.position();
        assert!(!ctx.ensure_flat_idx(6, &mut reader, &mut codec).unwrap());
        assert!(!ctx.ensure_flat_idx(100, &mut reader, &mut codec).unwrap());
        assert_eq!(reader.position(), pos_eof, "reads occurred past flist_eof");
    }

    #[test]
    fn ensure_all_segments_loaded_drains_every_segment() {
        let segments = vec![vec![("s0/a", 1u64)], vec![("s1/b", 2), ("s1/c", 3)]];
        let (wire, total) = encode_segments(&segments);

        let mut ctx = inc_recurse_receiver();
        // Two parent dirs (s0, s1) were in the initial flist; seed the count so
        // dir_ndx 0 and 1 pass the fail-closed range check.
        ctx.dir_flist_used = segments.len();
        let mut reader = Cursor::new(wire);
        let mut codec = create_ndx_codec(PROTOCOL);

        ctx.ensure_all_segments_loaded(&mut reader, &mut codec)
            .unwrap();
        assert_eq!(ctx.file_list().len(), total);
        assert!(ctx.flist_eof);
    }

    /// Protocol-32 INC_RECURSE receiver configured for a `-a` pull: the compat
    /// flags mirror what an upstream daemon negotiates (all known bits, so
    /// varint entry flags and inline id names are in force) and owner/group
    /// preservation is on so the uid/gid + name fields decode.
    fn archive_inc_recurse_receiver() -> ReceiverContext {
        use crate::flags::ParsedServerFlags;
        let mut handshake = test_handshake();
        handshake.compat_flags = Some(CompatibilityFlags::ALL_KNOWN);
        let config = ServerConfig {
            role: ServerRole::Receiver,
            protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
            flag_string: "-logDtpre.iLsfxCIvu".to_owned(),
            flags: ParsedServerFlags {
                owner: true,
                group: true,
                links: true,
                times: true,
                perms: true,
                recursive: true,
                archive: true,
                ..ParsedServerFlags::default()
            },
            args: vec![OsString::from(".")],
            ..Default::default()
        };
        ReceiverContext::new_for_test(&handshake, config)
    }

    /// A real multi-segment INC_RECURSE sub-list stream captured verbatim from
    /// an upstream rsync 3.4.4 daemon answering an `i`-advertised `-a` pull of a
    /// 3-directory / 6-file tree (`{a,b,c}/{f1,f2}.txt`). The daemon packs the
    /// whole stream into one `MSG_DATA` frame:
    ///
    /// - the initial level-1 flist (`.`, `b`, `a`, `c` in readdir order),
    /// - the end-of-flist marker (varint `0` flag + varint `0` io_error),
    /// - three per-directory sub-lists, each introduced by
    ///   `write_ndx(NDX_FLIST_OFFSET - dir_ndx)` (a `0xFF`-led negative NDX),
    /// - `write_ndx(NDX_FLIST_EOF)` (`0xFF 0xFE 0x80 0x02 …`).
    ///
    /// This exercises the ACTUAL carry/framing boundary an upstream peer
    /// produces. An oc<->oc round-trip hides the bug because both ends share the
    /// encoder; only genuine upstream bytes catch a receiver that reads the
    /// `0xFF` `NDX_FLIST_OFFSET` marker as a varint entry-flags byte (which trips
    /// `overflow in read_varint`, since `int_byte_extra[0xFF >> 2] = 5 > 4`).
    ///
    /// upstream: flist.c:2152 `write_ndx(NDX_FLIST_OFFSET - dir_ndx)`,
    /// io.c:2243 `write_ndx()`, flist.c:2112 `write_end_of_flist()`.
    #[rustfmt::skip]
    const UPSTREAM_INC_RECURSE_FRAME: &[u8] = &[
        0xac, 0x01, 0x01, 0x2e, 0x00, 0x00, 0x10, 0x6a, 0x66, 0x1f, 0x52, 0xf0,
        0x6f, 0x84, 0x1b, 0x1d, 0xfd, 0x41, 0x00, 0x00, 0x83, 0xe8, 0x04, 0x6f,
        0x66, 0x65, 0x72, 0x83, 0xe8, 0x04, 0x6f, 0x66, 0x65, 0x72, 0xa0, 0x9a,
        0x01, 0x62, 0x00, 0x00, 0x10, 0xf0, 0x3d, 0x65, 0xd9, 0x1c, 0xa0, 0x9a,
        0x01, 0x61, 0x00, 0x00, 0x10, 0xf0, 0xd3, 0x92, 0x93, 0x1c, 0xa0, 0x9a,
        0x01, 0x63, 0x00, 0x00, 0x10, 0xf0, 0x6f, 0x84, 0x1b, 0x1d, 0x00, 0x00,
        0xff, 0x65, 0xa0, 0x98, 0x08, 0x61, 0x2f, 0x66, 0x31, 0x2e, 0x74, 0x78,
        0x74, 0x00, 0x0a, 0x00, 0xf0, 0xd3, 0x92, 0x93, 0x1c, 0xb4, 0x81, 0x00,
        0x00, 0xa0, 0xba, 0x03, 0x05, 0x32, 0x2e, 0x74, 0x78, 0x74, 0x00, 0x07,
        0x00, 0xf0, 0xd3, 0x92, 0x93, 0x1c, 0x00, 0x00, 0xff, 0x01, 0xa0, 0x9a,
        0x08, 0x62, 0x2f, 0x66, 0x31, 0x2e, 0x74, 0x78, 0x74, 0x00, 0x0a, 0x00,
        0xf0, 0x3d, 0x65, 0xd9, 0x1c, 0xa0, 0xba, 0x03, 0x05, 0x32, 0x2e, 0x74,
        0x78, 0x74, 0x00, 0x07, 0x00, 0xf0, 0x3d, 0x65, 0xd9, 0x1c, 0x00, 0x00,
        0xff, 0x01, 0xa0, 0x9a, 0x08, 0x63, 0x2f, 0x66, 0x31, 0x2e, 0x74, 0x78,
        0x74, 0x00, 0x0a, 0x00, 0xf0, 0x6f, 0x84, 0x1b, 0x1d, 0xa0, 0xba, 0x03,
        0x05, 0x32, 0x2e, 0x74, 0x78, 0x74, 0x00, 0x07, 0x00, 0xf0, 0x6f, 0x84,
        0x1b, 0x1d, 0x00, 0x00, 0xff, 0xfe, 0x80, 0x02, 0x00, 0x00,
    ];

    #[test]
    fn real_upstream_multisegment_sublist_decodes_as_segments() {
        let mut ctx = archive_inc_recurse_receiver();
        let mut reader = Cursor::new(UPSTREAM_INC_RECURSE_FRAME.to_vec());

        // Initial flist: the four level-1 entries decode, the end-of-list marker
        // is consumed, but INC_RECURSE leaves `flist_eof` clear until the
        // terminating NDX_FLIST_EOF is seen in the sub-list stream.
        let initial = ctx
            .receive_file_list(&mut reader)
            .expect("initial level-1 flist decodes cleanly");
        assert_eq!(initial, 4, "level-1 flist has `.` plus dirs a, b, c");
        assert!(
            !ctx.flist_eof,
            "INC_RECURSE keeps flist_eof clear until NDX_FLIST_EOF"
        );

        // Drain the sub-lists. The regression: the `0xFF`-led NDX_FLIST_OFFSET
        // markers must be decoded as segment markers by `read_ndx`, NOT read as
        // varint entry flags. A fresh codec here matches the sender's fresh NDX
        // state at the first sub-list marker.
        let mut codec = create_ndx_codec(PROTOCOL);
        ctx.ensure_all_segments_loaded(&mut reader, &mut codec)
            .expect("NDX_FLIST_OFFSET sub-list markers decode as segments, not varint flags");

        assert!(
            ctx.flist_eof,
            "NDX_FLIST_EOF terminates the sub-list stream"
        );
        assert_eq!(
            ctx.file_list().len(),
            10,
            "4 level-1 dirs + 6 files across 3 per-directory sub-lists"
        );

        let names: std::collections::BTreeSet<String> = ctx
            .file_list()
            .iter()
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();
        for expected in [
            "a/f1.txt", "a/f2.txt", "b/f1.txt", "b/f2.txt", "c/f1.txt", "c/f2.txt",
        ] {
            assert!(
                names.contains(expected),
                "sub-list entry {expected} missing from decoded list: {names:?}"
            );
        }
    }

    #[test]
    fn ensure_flat_idx_is_noop_without_inc_recurse() {
        // A non-INC_RECURSE receiver is already at flist_eof (set by
        // receive_file_list); ensure_flat_idx must never touch the reader.
        let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
        ctx.flist_eof = true;
        ctx.file_list
            .push(FileEntry::new_file("only.txt".into(), 7, 0o100644));

        // A reader that would error if read from, proving no wire access.
        let mut reader = Cursor::new(Vec::<u8>::new());
        let mut codec = create_ndx_codec(PROTOCOL);

        assert!(ctx.ensure_flat_idx(0, &mut reader, &mut codec).unwrap());
        assert!(!ctx.ensure_flat_idx(1, &mut reader, &mut codec).unwrap());
        assert_eq!(reader.position(), 0);
    }

    /// Encodes a single INC_RECURSE sub-list header framed for `dir_ndx`, its
    /// entries, and the `NDX_FLIST_EOF` terminator. Unlike `encode_segments`,
    /// the `dir_ndx` is caller-chosen so a malformed/out-of-range index can be
    /// forced onto the wire.
    fn encode_segment_with_dir_ndx(dir_ndx: i32, entries: &[(&str, u64)]) -> Vec<u8> {
        let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
        let mut writer = FileListWriter::new(protocol);
        let mut ndx_codec = create_ndx_codec(PROTOCOL);
        let mut wire = Vec::new();
        ndx_codec
            .write_ndx(&mut wire, NDX_FLIST_OFFSET - dir_ndx)
            .unwrap();
        for (name, size) in entries {
            let mut e = FileEntry::new_file(PathBuf::from(name), *size, 0o100644);
            e.set_mtime(1_700_000_000, 0);
            writer.write_entry(&mut wire, &e).unwrap();
        }
        writer.write_end(&mut wire, None).unwrap();
        ndx_codec.write_ndx(&mut wire, NDX_FLIST_EOF).unwrap();
        wire
    }

    /// A `dir_ndx` equal to, past, or absurdly beyond `dir_flist_used` is
    /// untrusted wire data that references a directory the receiver never saw.
    ///
    /// WHY: upstream `flist.c:2622-2626` aborts with `exit_cleanup(RERR_PROTOCOL)`
    /// on `dir_ndx >= dir_flist->used`. oc must fail closed - reject with a
    /// `ProtocolViolation` (RERR_PROTOCOL) and append nothing - rather than trust
    /// the sender's index or (for a huge value) panic on the framing arithmetic.
    #[test]
    fn out_of_range_sublist_dir_ndx_is_rejected_fail_closed() {
        for bad in [1i32, 5, 2_000_000_000] {
            let wire = encode_segment_with_dir_ndx(bad, &[("x/a.txt", 1)]);
            let mut ctx = inc_recurse_receiver();
            // Only dir_ndx 0 would be in range.
            ctx.dir_flist_used = 1;
            let err = ctx
                .receive_extra_file_lists(&mut Cursor::new(wire))
                .expect_err("out-of-range dir_ndx must be rejected, not appended");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert!(
                err.get_ref()
                    .and_then(|e| e.downcast_ref::<protocol::ProtocolViolation>())
                    .is_some(),
                "range rejection must map to RERR_PROTOCOL, got {err:?}"
            );
            assert!(
                err.to_string().contains("refusing invalid dir_ndx"),
                "unexpected message: {err}"
            );
            assert_eq!(
                ctx.file_list().len(),
                0,
                "no entries may be appended when the header is rejected"
            );
        }
    }

    /// A second sub-list for a directory already served is a malicious duplicate.
    ///
    /// WHY: upstream `flist.c:2627-2632` sets `FLAG_GOT_DIR_FLIST` and aborts with
    /// `RERR_PROTOCOL` on the second sub-list; without the guard a sender could
    /// replay sub-lists to grow `file_list` without bound. The first sub-list for
    /// dir_ndx 0 is accepted, the second is refused.
    #[test]
    fn duplicate_sublist_for_same_dir_is_rejected() {
        let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
        let mut writer = FileListWriter::new(protocol);
        let mut ndx_codec = create_ndx_codec(PROTOCOL);
        let mut wire = Vec::new();
        for entries in [&[("dir0/a.txt", 1u64)][..], &[("dir0/b.txt", 2u64)][..]] {
            // Both headers target dir_ndx 0.
            ndx_codec.write_ndx(&mut wire, NDX_FLIST_OFFSET).unwrap();
            for (name, size) in entries {
                let mut e = FileEntry::new_file(PathBuf::from(name), *size, 0o100644);
                e.set_mtime(1_700_000_000, 0);
                writer.write_entry(&mut wire, &e).unwrap();
            }
            writer.write_end(&mut wire, None).unwrap();
        }
        ndx_codec.write_ndx(&mut wire, NDX_FLIST_EOF).unwrap();

        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist_used = 1;
        let err = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect_err("duplicate sub-list for dir 0 must be rejected");
        assert!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<protocol::ProtocolViolation>())
                .is_some(),
            "duplicate rejection must map to RERR_PROTOCOL, got {err:?}"
        );
        assert!(
            err.to_string()
                .contains("refusing malicious duplicate flist for dir 0"),
            "unexpected message: {err}"
        );
    }

    /// An in-range, non-duplicate `dir_ndx` sub-list is accepted normally - the
    /// guards must not reject legitimate wire data.
    #[test]
    fn in_range_sublist_dir_ndx_is_accepted() {
        let wire = encode_segment_with_dir_ndx(0, &[("dir0/a.txt", 3u64), ("dir0/b.txt", 4)]);
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist_used = 1;
        ctx.dir_flist_names = vec![PathBuf::from("dir0")];
        let n = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("in-range dir_ndx must be accepted");
        assert_eq!(n, 2);
        assert_eq!(ctx.file_list().len(), 2);
        assert!(ctx.flist_eof);
    }

    /// A sub-list that repeats a normalized name must collapse to a single entry.
    ///
    /// WHY: upstream runs `flist_sort_and_clean()` on EACH INC_RECURSE sub-list
    /// (send `flist.c:2190`, recv `flist.c:2771`), whose clean pass
    /// (`flist.c:3031`, active for the receiver) removes duplicate names. Before
    /// this fix `receive_one_extra_segment` sorted the sub-list but skipped the
    /// dedup, so a redundant or hostile duplicate survived and the generator
    /// requested the same path twice - an NDX divergence driven by untrusted
    /// bytes, exactly the class of bug the initial-list dedup (#6631) closed. The
    /// legitimate sender ships deduped sub-lists (partitions of an already-cleaned
    /// list), so this pass is a no-op there and only collapses a duplicate,
    /// keeping both sides' NDX numbering identical.
    #[test]
    fn sublist_duplicate_name_is_deduped() {
        let wire = encode_segment_with_dir_ndx(
            0,
            &[("x/dup.txt", 10u64), ("x/dup.txt", 10), ("x/z.txt", 20)],
        );
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist_used = 1;
        ctx.dir_flist_names = vec![PathBuf::from("x")];
        let n = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("legitimate sub-list must be accepted");
        // Three entries arrive on the wire; the duplicate "x/dup.txt" collapses.
        assert_eq!(n, 3, "the wire carried three entries");
        let names: Vec<String> = ctx
            .file_list()
            .iter()
            .map(|e| e.name().to_owned())
            .collect();
        assert_eq!(
            names,
            vec!["x/dup.txt".to_owned(), "x/z.txt".to_owned()],
            "the repeated name must be deduped, leaving two entries"
        );
    }

    /// A sub-list entry whose dirname escapes its declared parent must be rejected.
    ///
    /// WHY: upstream `flist.c:2684-2695` compares every sub-list entry's dirname
    /// against `f_name(dir_flist->files[dir_ndx])` and, on a mismatch, aborts with
    /// `exit_cleanup(RERR_UNSUPPORTED)` ("ABORTING due to invalid path from
    /// sender"). Without this check a hostile sender could frame a sub-list for a
    /// legitimate parent (`dir_ndx` 0 = "x") but fill it with an entry that lands
    /// outside that tree ("y/evil.txt"), escaping the intended directory. The
    /// range/duplicate guards (#28) do not catch this because `dir_ndx` itself is
    /// valid; only the path-belongs check does.
    #[test]
    fn sublist_entry_escaping_parent_is_rejected() {
        let wire = encode_segment_with_dir_ndx(0, &[("y/evil.txt", 9u64)]);
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist_used = 1;
        // dir_ndx 0 is the legitimate parent "x"; the entry claims "y".
        ctx.dir_flist_names = vec![PathBuf::from("x")];
        let err = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect_err("an entry escaping its parent must be rejected");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::Unsupported,
            "path-belongs rejection must map to RERR_UNSUPPORTED (4), got {err:?}"
        );
        assert!(
            err.to_string()
                .contains("ABORTING due to invalid path from sender"),
            "unexpected message: {err}"
        );
        assert_eq!(
            ctx.file_list().len(),
            0,
            "the escaping segment's entries must be dropped"
        );
    }

    /// A legitimate entry that lives directly under its declared parent passes.
    ///
    /// WHY: the path-belongs guard must accept the normal case (entry dirname ==
    /// parent) or it would break every deep INC_RECURSE transfer. Guards this
    /// against a false-positive regression.
    #[test]
    fn sublist_entry_under_parent_is_accepted() {
        let wire = encode_segment_with_dir_ndx(0, &[("x/a.txt", 1u64), ("x/b.txt", 2)]);
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist_used = 1;
        ctx.dir_flist_names = vec![PathBuf::from("x")];
        let n = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("entries under their parent must be accepted");
        assert_eq!(n, 2);
        assert_eq!(ctx.file_list().len(), 2);
    }
}
