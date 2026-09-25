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
//! - [`ReceiverContext::read_next_frame`] classifies and dispatches one frame
//!   through the shared marker-aware reader
//!   ([`crate::receiver::ndx_stream::read_ndx_step`]), which appends a segment
//!   via [`receive_one_extra_segment`](ReceiverContext::receive_one_extra_segment)
//!   and sets `flist_eof` at the terminator.
//! - [`ReceiverContext::ensure_flat_idx`] pulls segments until a target flat
//!   index is materialized (or the list ends), never indexing out of bounds.
//! - [`ReceiverContext::ensure_all_segments_loaded`] drains every remaining
//!   segment in one pass - the explicit whole-list fallback for the callers that
//!   have no per-file cursor to pull segments through (the recorded-stream replay
//!   driver and `receive_extra_file_lists`). The live pipelined drivers walk the
//!   same segments through `ensure_flat_idx`, like the synchronous driver.
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
//! - `generator.c:2316-2385` - `generate_files()` on-demand fetch loop

use std::io::{self, Read};

use protocol::codec::NdxCodecEnum;

use super::super::ReceiverContext;
use super::super::ndx_stream::{NdxStep, read_ndx_step};

impl ReceiverContext {
    /// Reads and dispatches one frame off the sender's stream.
    ///
    /// A thin adapter over [`read_ndx_step`], the shared marker-aware reader:
    /// the receiver *is* the lazy file-list sink, so a segment marker is
    /// consumed in full via
    /// [`receive_one_extra_segment`](Self::receive_one_extra_segment) and
    /// `NDX_FLIST_EOF` sets `flist_eof`, both as side effects of the step.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:329-381` - the `read_loop` this steps through
    pub(in crate::receiver) fn read_next_frame<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<NdxStep> {
        read_ndx_step(reader, ndx_codec, self)
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
    /// Encountering a per-file [`NdxStep::File`] while fetching a segment is
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
                NdxStep::Segment(_) | NdxStep::FlistEof | NdxStep::DelStats => {}
                NdxStep::Done => {
                    // The sender signalled completion before NDX_FLIST_EOF;
                    // treat it as the end of the list.
                    self.flist_eof = true;
                    return Ok(false);
                }
                NdxStep::File(ndx) => return Err(unexpected_reply(ndx)),
            }
        }
    }

    /// Drains every remaining INC_RECURSE segment until `flist_eof` in one pass.
    ///
    /// The explicit whole-list fallback for callers that consume the entire
    /// `file_list` at once and have no per-file cursor to pull segments through:
    /// the recorded-stream replay driver and `receive_extra_file_lists`
    /// (`--files-from` and the wire-parity tests). The live pipelined drivers
    /// walk the same segments through [`ensure_flat_idx`](Self::ensure_flat_idx).
    /// A no-op (no wire read) once `flist_eof` is set, which is always the case
    /// on a non-INC_RECURSE transfer by the time a caller reaches this.
    pub(in crate::receiver) fn ensure_all_segments_loaded<R: Read + ?Sized>(
        &mut self,
        reader: &mut R,
        ndx_codec: &mut NdxCodecEnum,
    ) -> io::Result<()> {
        while !self.flist_eof {
            match self.read_next_frame(reader, ndx_codec)? {
                NdxStep::Segment(_) | NdxStep::FlistEof | NdxStep::DelStats => {}
                NdxStep::Done => self.flist_eof = true,
                NdxStep::File(ndx) => return Err(unexpected_reply(ndx)),
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
                NdxStep::Segment(_) | NdxStep::FlistEof | NdxStep::DelStats => {}
                NdxStep::Done => self.flist_eof = true,
                // A per-file reply here means the transfer phase has started;
                // stop prefetching rather than treat it as a desync.
                NdxStep::File(_) => break,
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

    use protocol::codec::{
        NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum, create_ndx_codec,
    };

    use super::super::dir_flist::{DirFlist, DirSlot};
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
        // fail-closed `dir_ndx >= dir_flist.used()` range check.
        ctx.dir_flist = DirFlist::with_active((0..segments.len()).map(|i| format!("dir{i}")));
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

    /// Regression (#55): the receiver must read the sub-list markers through the
    /// SAME inbound codec it reads transfer echoes with. Upstream io.c keeps one
    /// `read_ndx` state per `f_in`; each `NDX_FLIST_OFFSET` marker is diff-encoded
    /// against a running `prev_negative`, so a codec that did not see the earlier
    /// markers decodes the next one against a stale base and misframes the stream.
    /// The pre-fix streaming driver pulled sub-lists on a separate `flist_ndx_codec`
    /// while echoes used `ndx_read_codec`; once the two interleaved, the echo codec
    /// mis-decoded an `NDX_FLIST_OFFSET` marker as `NDX_FLIST_EOF` and read the tail
    /// as a bogus file index ("sender echoed NDX 159" live). This pins the invariant
    /// at the read primitive: one shared codec loads the whole list; a fresh codec
    /// handed the second pull cannot reproduce that clean state.
    #[test]
    fn interleaved_segment_pulls_require_one_shared_inbound_codec() {
        let segments = vec![
            vec![("dir0/a.txt", 10u64), ("dir0/b.txt", 20)],
            vec![("dir1/c.txt", 30), ("dir1/d.txt", 40)],
            vec![("dir2/e.txt", 50)],
        ];
        let (wire, total) = encode_segments(&segments);

        // Runs the SAME two-pull sequence - segment 0 first, then a second pull
        // for the rest - and reports whether the whole list materialized. The
        // ONLY variable between the two calls below is whether the second pull
        // reuses the first pull's codec (shared inbound state) or gets a fresh
        // one (split state); everything else is identical, so the outcome
        // isolates codec-sharing as the cause (mutation probe, not a vacuous
        // pass).
        fn two_pull_reaches_full_list(
            wire: &[u8],
            n_dirs: usize,
            total: usize,
            split: bool,
        ) -> bool {
            let mut ctx = inc_recurse_receiver();
            ctx.dir_flist = DirFlist::with_active((0..n_dirs).map(|i| format!("dir{i}")));
            let mut reader = Cursor::new(wire.to_vec());
            let mut codec_a = create_ndx_codec(PROTOCOL);
            let mut codec_b = create_ndx_codec(PROTOCOL);
            // First pull: segment 0 (2 entries) via codec A.
            if !ctx.ensure_flat_idx(0, &mut reader, &mut codec_a).unwrap()
                || ctx.file_list().len() != 2
            {
                return false;
            }
            // Second pull: the rest. Shared reuses codec A (upstream's one
            // read_ndx state); split hands it a fresh codec B whose prev_negative
            // never saw segment 0's marker.
            let second = if split { &mut codec_b } else { &mut codec_a };
            ctx.ensure_flat_idx(total - 1, &mut reader, second)
                .map(|_| ctx.file_list().len() == total)
                .unwrap_or(false)
        }

        // Shared inbound codec across both pulls: the full list materializes.
        assert!(
            two_pull_reaches_full_list(&wire, segments.len(), total, false),
            "one shared inbound codec must decode every interleaved segment"
        );
        // Fresh codec at the second pull (the pre-fix two-codec split): it
        // mis-decodes segment 1's diff-encoded marker and cannot reach the full
        // list. If this ever passes, the invariant has regressed to two codecs.
        assert!(
            !two_pull_reaches_full_list(&wire, segments.len(), total, true),
            "a fresh inbound codec at the second pull must desync, not reproduce \
             the shared-codec full-list read"
        );
    }

    #[test]
    fn ensure_all_segments_loaded_drains_every_segment() {
        let segments = vec![vec![("s0/a", 1u64)], vec![("s1/b", 2), ("s1/c", 3)]];
        let (wire, total) = encode_segments(&segments);

        let mut ctx = inc_recurse_receiver();
        // Two parent dirs (s0, s1) were in the initial flist; seed them so
        // dir_ndx 0 and 1 pass the fail-closed range check.
        ctx.dir_flist = DirFlist::with_active((0..segments.len()).map(|i| format!("s{i}")));
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
    /// io.c:2318 `write_ndx()`, flist.c:2112 `write_end_of_flist()`.
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

        // The SEGMENT COUNT is the point, and nothing above checks it: every
        // other assertion in this test holds identically if all six files
        // arrived in ONE segment, so "across 3 per-directory sub-lists" was a
        // claim in a message rather than a checked fact. This is also what makes
        // the "received segment" debug line non-vacuous - that line reports
        // `ndx_segments.len()`, so pinning the count here pins what it prints.
        assert_eq!(
            ctx.ndx_segments.len(),
            4,
            "initial level-1 list plus one segment per sub-list (a, b, c)"
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

    /// The two pipelined drivers materialize the flist by walking a flat cursor
    /// through `ensure_flat_idx` (like `sync.rs`) instead of the
    /// `ensure_all_segments_loaded` drain. That swap is only sound if the cursor
    /// walk reaches the SAME terminal state as the drain: identical entries,
    /// identical segment count, `flist_eof` set, and the whole frame consumed
    /// with no over- or under-read. Runs both arms over the real upstream
    /// multi-segment frame and asserts the observable state is identical. A
    /// future change that let `ensure_flat_idx` diverge from the drain (skip a
    /// segment, over-read past the terminator, or stop short) fails here rather
    /// than silently changing what a live INC_RECURSE pull loads - the single
    /// non-obvious risk the driver conversion took on.
    #[test]
    fn cursor_walk_materializes_identically_to_the_drain() {
        // Drain arm: the retired `ensure_all_segments_loaded` path the replay
        // and `--files-from` callers still use.
        let mut drained = archive_inc_recurse_receiver();
        let mut drain_reader = Cursor::new(UPSTREAM_INC_RECURSE_FRAME.to_vec());
        drained
            .receive_file_list(&mut drain_reader)
            .expect("initial level-1 flist decodes cleanly");
        let mut drain_codec = create_ndx_codec(PROTOCOL);
        drained
            .ensure_all_segments_loaded(&mut drain_reader, &mut drain_codec)
            .expect("drain materializes every segment");

        // Cursor arm: the flat-index walk the pipelined drivers now perform.
        let mut walked = archive_inc_recurse_receiver();
        let mut walk_reader = Cursor::new(UPSTREAM_INC_RECURSE_FRAME.to_vec());
        walked
            .receive_file_list(&mut walk_reader)
            .expect("initial level-1 flist decodes cleanly");
        let mut walk_codec = create_ndx_codec(PROTOCOL);
        let mut flat_idx = 0usize;
        while walked
            .ensure_flat_idx(flat_idx, &mut walk_reader, &mut walk_codec)
            .expect("cursor pulls each segment on demand")
        {
            flat_idx += 1;
        }

        assert!(
            walked.flist_eof && drained.flist_eof,
            "both arms must reach the NDX_FLIST_EOF terminator"
        );
        assert_eq!(
            walked.file_list().len(),
            drained.file_list().len(),
            "cursor and drain must materialize the same entry count"
        );
        assert_eq!(
            walked.ndx_segments.len(),
            drained.ndx_segments.len(),
            "cursor and drain must record the same segment count"
        );
        let names = |ctx: &ReceiverContext| -> Vec<String> {
            ctx.file_list()
                .iter()
                .map(|e| e.path().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            names(&walked),
            names(&drained),
            "cursor and drain must materialize identical entries in identical order"
        );
        // The cursor stops exactly at the terminator: it advanced to one past
        // the last entry, and both arms consumed the whole frame to the same
        // byte - no over-read (which would desync a live transfer) and no
        // under-read (which would leave a segment pending).
        assert_eq!(flat_idx, walked.file_list().len());
        assert_eq!(
            walk_reader.position(),
            drain_reader.position(),
            "cursor and drain must consume the same wire bytes"
        );
    }

    /// `--debug=flist2` emissions across the REAL upstream multi-segment
    /// stream above: the initial list prints one `recv_file_name(%s)` per
    /// entry (flist.c:3012), `received %d names` (flist.c:3019) and
    /// `recv_file_list done` (flist.c:3088); each sub-list adds
    /// `[receiver] receiving flist for dir %d` (rsync.c:373) plus its own
    /// name/count/done set. Verbosity is installed through the same
    /// `apply_debug_flag` funnel the CLI's `--debug=` parser uses. Exact
    /// sequence, so a silenced or duplicated site cannot pass.
    #[test]
    fn upstream_multisegment_sublists_emit_flist2_parity_lines() {
        logging::init(logging::VerbosityConfig::default());
        logging::apply_debug_flag("flist2").expect("flist2 parses");
        let _ = logging::drain_events();

        let mut ctx = archive_inc_recurse_receiver();
        let mut reader = Cursor::new(UPSTREAM_INC_RECURSE_FRAME.to_vec());
        ctx.receive_file_list(&mut reader)
            .expect("initial level-1 flist decodes cleanly");
        let mut codec = create_ndx_codec(PROTOCOL);
        ctx.ensure_all_segments_loaded(&mut reader, &mut codec)
            .expect("sub-list stream drains");

        let messages: Vec<String> = logging::drain_events()
            .into_iter()
            .filter_map(|event| match event {
                logging::DiagnosticEvent::Debug {
                    flag: logging::DebugFlag::Flist,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect();
        let expected: Vec<String> = [
            // Initial level-1 list, wire (readdir) order.
            "recv_file_name(.)",
            "recv_file_name(b)",
            "recv_file_name(a)",
            "recv_file_name(c)",
            "received 4 names",
            "recv_file_list done",
            // One sub-list per directory, framed by its dir_ndx.
            "[receiver] receiving flist for dir 1",
            "recv_file_name(a/f1.txt)",
            "recv_file_name(a/f2.txt)",
            "received 2 names",
            "recv_file_list done",
            "[receiver] receiving flist for dir 2",
            "recv_file_name(b/f1.txt)",
            "recv_file_name(b/f2.txt)",
            "received 2 names",
            "recv_file_list done",
            "[receiver] receiving flist for dir 3",
            "recv_file_name(c/f1.txt)",
            "recv_file_name(c/f2.txt)",
            "received 2 names",
            "recv_file_list done",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
        assert_eq!(messages, expected);
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
        let entries: Vec<FileEntry> = entries
            .iter()
            .map(|(name, size)| FileEntry::new_file(PathBuf::from(name), *size, 0o100644))
            .collect();
        encode_entries_with_dir_ndx(dir_ndx, &entries)
    }

    /// Frames an INC_RECURSE sub-list around caller-built entries, so a test can
    /// ship directories as well as regular files. `encode_segment_with_dir_ndx`
    /// is the regular-file shorthand over this.
    fn encode_entries_with_dir_ndx(dir_ndx: i32, entries: &[FileEntry]) -> Vec<u8> {
        let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
        let mut writer = FileListWriter::new(protocol);
        let mut ndx_codec = create_ndx_codec(PROTOCOL);
        let mut wire = Vec::new();
        append_segment(&mut wire, &mut writer, &mut ndx_codec, dir_ndx, entries);
        ndx_codec.write_ndx(&mut wire, NDX_FLIST_EOF).unwrap();
        wire
    }

    /// Appends one sub-list segment (header `dir_ndx` + entries + end marker)
    /// WITHOUT the stream terminator, so a caller can frame several segments
    /// back to back. The writer and NDX codec are borrowed rather than created
    /// per segment because both carry incremental-encoding state that upstream
    /// also carries across the whole sub-list stream; a fresh codec per segment
    /// would encode bytes no real sender emits.
    fn append_segment(
        wire: &mut Vec<u8>,
        writer: &mut FileListWriter,
        ndx_codec: &mut NdxCodecEnum,
        dir_ndx: i32,
        entries: &[FileEntry],
    ) {
        ndx_codec
            .write_ndx(wire, NDX_FLIST_OFFSET - dir_ndx)
            .unwrap();
        for entry in entries {
            let mut e = entry.clone();
            e.set_mtime(1_700_000_000, 0);
            writer.write_entry(wire, &e).unwrap();
        }
        writer.write_end(wire, None).unwrap();
    }

    /// A `dir_ndx` equal to, past, or absurdly beyond `dir_flist.used()` is
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
            ctx.dir_flist = DirFlist::with_active(["x"]);
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
        ctx.dir_flist = DirFlist::with_active(["dir0"]);
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
        ctx.dir_flist = DirFlist::with_active(["dir0"]);
        let n = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("in-range dir_ndx must be accepted");
        assert_eq!(n, 2);
        assert_eq!(ctx.file_list().len(), 2);
        assert!(ctx.flist_eof);
    }

    /// A sub-list that repeats a normalized name must TOMBSTONE the duplicate
    /// in place, preserving the slot count so NDX stays aligned.
    ///
    /// WHY: upstream runs `flist_sort_and_clean()` on EACH INC_RECURSE sub-list
    /// (send `flist.c:2190`, recv `flist.c:2771`), whose clean pass
    /// (`flist.c:3031`, active for the receiver) drops duplicate names by
    /// `clear_file()` (`flist.c:3089`) - a tombstone that keeps the entry's
    /// array slot so following NDX values are unaffected. The receiver must NOT
    /// compact or renumber, or its numbering desyncs from the sender's full
    /// un-deduped array. The legitimate sender ships un-deduped sub-lists (a
    /// non-incremental sender skips its clean), so the receiver's tombstone is
    /// what keeps both sides' NDX numbering identical.
    #[test]
    fn sublist_duplicate_name_is_tombstoned() {
        let wire = encode_segment_with_dir_ndx(
            0,
            &[("x/dup.txt", 10u64), ("x/dup.txt", 10), ("x/z.txt", 20)],
        );
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist = DirFlist::with_active(["x"]);
        let n = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("legitimate sub-list must be accepted");
        // Three entries arrive on the wire; all three slots are preserved.
        assert_eq!(n, 3, "the wire carried three entries");
        assert_eq!(
            ctx.file_list().len(),
            3,
            "every NDX slot is preserved (tombstone, not compact)"
        );
        // The middle slot is a tombstone (inactive); NDX 0 and 2 stay put.
        assert!(ctx.file_list()[0].is_active());
        assert!(!ctx.file_list()[1].is_active());
        assert!(ctx.file_list()[2].is_active());
        let active: Vec<String> = ctx
            .file_list()
            .iter()
            .filter(|e| e.is_active())
            .map(|e| e.name().to_owned())
            .collect();
        assert_eq!(
            active,
            vec!["x/dup.txt".to_owned(), "x/z.txt".to_owned()],
            "the repeated name is dropped, leaving two active entries"
        );
    }

    /// The `file_list` tombstone above keeps NDX aligned, and `dir_flist` - a
    /// SECOND, independent numbering - must keep its own slot for the same
    /// tombstoned directory.
    ///
    /// Upstream keeps the slot: the receiver appends every directory to
    /// `dir_flist` as it READS it (`flist.c:2996-2998`, before any clean), and
    /// `dir_flist->files[]` holds POINTERS into the same `file_struct`s as the
    /// transfer list. When `flist_sort_and_clean()` later `clear_file()`s the
    /// duplicate, the shared struct is zeroed but the `dir_flist` slot remains,
    /// now inactive - which is precisely the slot `flist.c:2911-2918` refuses
    /// with "refusing flist for cleared dir_ndx %d".
    ///
    /// [`DirFlist`] reproduces that by recording the directories BEFORE the
    /// clean and marking the ones it tombstoned.
    #[test]
    fn duplicate_directory_keeps_its_dir_flist_slot() {
        let entries = [
            FileEntry::new_directory(PathBuf::from("x/d"), 0o755),
            FileEntry::new_directory(PathBuf::from("x/d"), 0o755),
            FileEntry::new_file(PathBuf::from("x/z.txt"), 20, 0o100644),
        ];
        let wire = encode_entries_with_dir_ndx(0, &entries);
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist = DirFlist::with_active(["x"]);

        ctx.receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("legitimate sub-list must be accepted");

        // Upstream: the parent `x` plus BOTH `x/d` slots, the second inactive.
        assert_eq!(
            ctx.dir_flist.used(),
            3,
            "the tombstoned duplicate directory must keep its dir_flist slot"
        );
        assert_eq!(
            ctx.dir_flist.resolve(1),
            Some(&DirSlot::Active(PathBuf::from("x/d"))),
            "the surviving duplicate keeps the slot"
        );
        assert_eq!(
            ctx.dir_flist.resolve(2),
            Some(&DirSlot::Cleared),
            "the tombstoned duplicate's slot survives as inactive"
        );
    }

    /// The dense numbering above is not merely a missing diagnostic: when the
    /// tombstoned directory is NOT the last one, every later `dir_ndx` shifts
    /// down by one, so a hostile index that upstream refuses as CLEARED lands on
    /// a DIFFERENT, live directory in oc and the sub-list is accepted.
    ///
    /// Fixture: parent `x` (dir_ndx 0) with children `a`, `d`, `d`, `z`.
    ///
    /// | dir_ndx | upstream receiver | oc receiver |
    /// |---|---|---|
    /// | 0 | `x`            | `x`   |
    /// | 1 | `x/a`          | `x/a` |
    /// | 2 | `x/d`          | `x/d` |
    /// | 3 | `x/d` CLEARED  | `x/z` |
    ///
    /// Upstream refuses `dir_ndx` 3 at `flist.c:2911-2918`. oc's bounds check
    /// (`flist.c:2906-2909`) cannot: 3 is in range. The peer's entries are
    /// grafted under `x/z` instead. The `proto-cleared-dirflist` cell happens to
    /// put the duplicate LAST, where the bounds check does catch it - so that
    /// cell alone understates the defect.
    #[test]
    fn a_cleared_directory_before_others_shifts_every_later_dir_ndx() {
        let entries = [
            FileEntry::new_directory(PathBuf::from("x/a"), 0o755),
            FileEntry::new_directory(PathBuf::from("x/d"), 0o755),
            FileEntry::new_directory(PathBuf::from("x/d"), 0o755),
            FileEntry::new_directory(PathBuf::from("x/z"), 0o755),
        ];
        let wire = encode_entries_with_dir_ndx(0, &entries);
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist = DirFlist::with_active(["x"]);

        ctx.receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("legitimate sub-list must be accepted");

        // Upstream: 5 slots, index 3 inactive. Measuring what oc resolves index
        // 3 to is the whole point - a name here means the hostile index was
        // silently re-pointed rather than refused.
        assert_eq!(
            ctx.dir_flist.resolve(3),
            Some(&DirSlot::Cleared),
            "dir_ndx 3 must still be the cleared duplicate, not a later sibling"
        );
        assert_eq!(
            ctx.dir_flist.resolve(4),
            Some(&DirSlot::Active(PathBuf::from("x/z"))),
            "the sibling keeps its own, later slot"
        );
    }

    /// A sub-list entry whose dirname escapes its declared parent must be rejected.
    ///
    /// WHY: upstream `flist.c:2719-2730` compares every sub-list entry's dirname
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
        // dir_ndx 0 is the legitimate parent "x"; the entry claims "y".
        ctx.dir_flist = DirFlist::with_active(["x"]);
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
        ctx.dir_flist = DirFlist::with_active(["x"]);
        let n = ctx
            .receive_extra_file_lists(&mut Cursor::new(wire))
            .expect("entries under their parent must be accepted");
        assert_eq!(n, 2);
        assert_eq!(ctx.file_list().len(), 2);
    }

    /// RS-3a: a metadata-only itemize echo (no `ITEM_TRANSFER`) can be PRECEDED
    /// by an INC_RECURSE sub-list segment marker once the eager flist drain is
    /// removed - the metadata-only rows are merged into the same request stream
    /// as the transfers, so the sender may emit a segment header between two
    /// replies. The old positional read at `pipeline.rs:847`
    /// (`SenderAttrs::read_with_codec_xattr`) reads the FIRST NDX, which is the
    /// `NDX_FLIST_OFFSET` marker, never consumes the segment body, and hands the
    /// caller the marker's negative NDX - a desync. The marker-aware read
    /// (`read_ndx_and_attrs`, what `:847` now uses) dispatches the segment via
    /// the receiver sink and returns the real echo NDX. Both arms run the SAME
    /// bytes, so the contrast is the whole point.
    ///
    /// upstream: rsync.c:322-431 `read_ndx_and_attrs()` - the marker loop the
    /// positional helper lacks.
    #[test]
    fn no_transfer_echo_read_absorbs_an_interleaved_segment_marker() {
        use crate::receiver::ndx_stream::read_ndx_and_attrs;
        use crate::receiver::wire::SenderAttrs;

        // A metadata-only echo: a positive NDX then a 2-byte iflags with no
        // ITEM_TRANSFER and no *_FOLLOWS bit, so there is no attribute tail.
        const ECHO_NDX: i32 = 1;
        let echo_iflags = SenderAttrs::ITEM_LOCAL_CHANGE;

        // [one sub-list segment for dir_ndx 0][the metadata-only echo].
        let build_wire = || -> Vec<u8> {
            let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
            let mut writer = FileListWriter::new(protocol);
            let mut codec = create_ndx_codec(PROTOCOL);
            let mut wire = Vec::new();
            let entries = [FileEntry::new_file(PathBuf::from("x/a.txt"), 1, 0o100644)];
            append_segment(&mut wire, &mut writer, &mut codec, 0, &entries);
            // Echo written with the SAME codec so its diff-state follows the
            // marker, exactly as a real sender interleaves them.
            codec.write_ndx(&mut wire, ECHO_NDX).unwrap();
            wire.extend_from_slice(&echo_iflags.to_le_bytes());
            wire
        };

        // Arm A - the pre-RS-3a positional read: surfaces the marker's negative
        // NDX and leaves the segment body unconsumed (desync).
        let mut reader_a = Cursor::new(build_wire());
        let mut codec_a = create_ndx_codec(PROTOCOL);
        let (positional_ndx, _attrs) =
            SenderAttrs::read_with_codec_xattr(&mut reader_a, &mut codec_a, false, false)
                .expect("positional read decodes an NDX");
        assert!(
            positional_ndx < 0,
            "positional read surfaces the segment marker as a negative NDX (desync), got {positional_ndx}"
        );
        assert_ne!(
            positional_ndx, ECHO_NDX,
            "positional read must NOT return the echo NDX - that is the desync being fixed"
        );

        // Arm B - the marker-aware read: dispatches the segment (file_list
        // grows) and returns the real echo NDX, whole wire consumed.
        let mut ctx = inc_recurse_receiver();
        ctx.dir_flist = DirFlist::with_active(["x"]);
        let before = ctx.file_list().len();
        let mut reader_b = Cursor::new(build_wire());
        let wire_len = reader_b.get_ref().len() as u64;
        let mut codec_b = create_ndx_codec(PROTOCOL);
        let got = read_ndx_and_attrs(&mut reader_b, &mut codec_b, &mut ctx, false, false)
            .expect("marker-aware read succeeds");
        assert_eq!(
            got.map(|(ndx, _)| ndx),
            Some(ECHO_NDX),
            "marker-aware read must absorb the segment and return the echo NDX"
        );
        assert_eq!(
            ctx.file_list().len(),
            before + 1,
            "the interleaved segment's single entry must be appended by the sink"
        );
        assert_eq!(
            reader_b.position(),
            wire_len,
            "marker-aware read must consume the whole [segment][echo] frame, no under/over-read"
        );
    }
}
