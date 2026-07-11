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
        let mut reader = Cursor::new(wire);
        let mut codec = create_ndx_codec(PROTOCOL);

        ctx.ensure_all_segments_loaded(&mut reader, &mut codec)
            .unwrap();
        assert_eq!(ctx.file_list().len(), total);
        assert!(ctx.flist_eof);
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
}
