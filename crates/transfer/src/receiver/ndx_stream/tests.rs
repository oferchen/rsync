//! Branch-order tests for the shared marker-aware NDX reader.
//!
//! Every case here pins a specific line of upstream's `read_loop`
//! (`rsync.c:329-431`). They are written so that *reordering* the branches
//! fails them, not merely changing what each branch does: the `NDX_DEL_STATS`
//! cases run against a sink whose `inc_recurse` is off, which only passes while
//! `rsync.c:336-342` is evaluated ahead of the `rsync.c:343` gate, and the
//! `NDX_DONE` case asserts the reader position so a stray attribute read is
//! visible.
//!
//! Wire bytes are built with the same encoders the receiver's other wire-parity
//! tests use (`create_ndx_codec` for framing, `FileListWriter` for sub-list
//! entries).

use std::ffi::OsString;
use std::io::Cursor;
use std::path::PathBuf;

use protocol::codec::{
    NDX_DEL_STATS, NDX_DONE, NDX_FLIST_EOF, NDX_FLIST_OFFSET, NdxCodec, NdxCodecEnum,
    create_ndx_codec,
};
use protocol::flist::{FileEntry, FileListWriter};
use protocol::stats::DeleteStats;
use protocol::{CompatibilityFlags, ProtocolVersion};

use super::*;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::receiver::file_list::DirFlist;
use crate::role::ServerRole;

const PROTOCOL: u8 = 32;

/// Protocol-32 receiver config with no flags set.
fn test_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

/// Protocol-32 handshake carrying `flags`.
fn test_handshake(flags: Option<CompatibilityFlags>) -> HandshakeResult {
    HandshakeResult {
        protocol: ProtocolVersion::try_from(PROTOCOL).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: flags,
        checksum_seed: 0,
    }
}

/// An INC_RECURSE receiver that has already seen `dirs` level-1 directories,
/// so their `dir_ndx` values pass the fail-closed range check.
fn inc_recurse_receiver(dirs: &[&str]) -> ReceiverContext {
    let mut ctx = ReceiverContext::new_for_test(
        &test_handshake(Some(CompatibilityFlags::INC_RECURSE)),
        test_config(),
    );
    ctx.dir_flist = DirFlist::with_active(dirs.iter().copied());
    ctx
}

/// Appends one sub-list segment for `dir_ndx` (header + entries + end marker).
fn push_segment(wire: &mut Vec<u8>, codec: &mut NdxCodecEnum, dir_ndx: i32, entries: &[&str]) {
    let protocol = ProtocolVersion::try_from(PROTOCOL).unwrap();
    let mut writer = FileListWriter::new(protocol);
    codec
        .write_ndx(wire, NDX_FLIST_OFFSET - dir_ndx)
        .expect("segment header encodes");
    for name in entries {
        let mut entry = FileEntry::new_file(PathBuf::from(*name), 8, 0o100644);
        entry.set_mtime(1_700_000_000, 0);
        writer.write_entry(wire, &entry).expect("entry encodes");
    }
    writer.write_end(wire, None).expect("end marker encodes");
}

/// Appends a per-file echo: NDX plus the two-byte protocol >= 29 `iflags` word.
fn push_file_echo(wire: &mut Vec<u8>, codec: &mut NdxCodecEnum, ndx: i32, iflags: u16) {
    codec.write_ndx(wire, ndx).expect("file ndx encodes");
    wire.extend_from_slice(&iflags.to_le_bytes());
}

/// Appends an `NDX_DEL_STATS` frame carrying `stats`.
fn push_del_stats(wire: &mut Vec<u8>, codec: &mut NdxCodecEnum, stats: &DeleteStats) {
    codec
        .write_ndx(wire, NDX_DEL_STATS)
        .expect("del-stats marker encodes");
    stats.write_to(wire).expect("del-stats varints encode");
}

/// A sub-list marker arriving BETWEEN two per-file echoes is consumed in full;
/// only the file indices surface to the caller.
///
/// WHY: upstream `rsync.c:360-380` handles the marker inside `read_loop` and
/// `continue`s, so the sender may interleave sub-list traffic with per-file
/// echoes at any point. A reader that surfaced the marker (or that decoded its
/// leading `0xFF` byte as an `iflags` word) would desync the transfer stream.
#[test]
fn segment_between_file_echoes_is_consumed_and_never_surfaces() {
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    push_file_echo(&mut wire, &mut codec, 3, 0);
    push_segment(&mut wire, &mut codec, 0, &["d0/a.txt", "d0/b.txt"]);
    push_file_echo(&mut wire, &mut codec, 4, 0);

    let mut ctx = inc_recurse_receiver(&["d0"]);
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);

    let (first, _) = read_ndx_and_attrs(&mut reader, &mut read_codec, &mut ctx, false, false)
        .expect("first echo decodes")
        .expect("a per-file index, not NDX_DONE");
    assert_eq!(first, 3);
    assert_eq!(ctx.file_list().len(), 0, "no segment consumed yet");

    let (second, _) = read_ndx_and_attrs(&mut reader, &mut read_codec, &mut ctx, false, false)
        .expect("the interleaved segment is consumed, then the next echo decodes")
        .expect("a per-file index, not NDX_DONE");
    assert_eq!(second, 4, "only file NDXs surface");
    assert_eq!(
        ctx.file_list().len(),
        2,
        "the interleaved sub-list was appended as a side effect"
    );
    assert!(!ctx.flist_eof);
}

/// `NDX_FLIST_EOF` mid-transfer sets `flist_eof` and does not surface.
///
/// WHY: upstream `rsync.c:353-359` sets the flag and `continue`s - it never
/// leaves the loop - so the caller's next value must be the following frame.
/// The receiver relies on this flag to stop pulling segments; surfacing the
/// marker instead would strand the lazy fetch loop.
#[test]
fn flist_eof_sets_the_flag_and_continues() {
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    codec.write_ndx(&mut wire, NDX_FLIST_EOF).unwrap();
    push_file_echo(&mut wire, &mut codec, 7, 0);

    let mut ctx = inc_recurse_receiver(&["d0"]);
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);

    let (ndx, _) = read_ndx_and_attrs(&mut reader, &mut read_codec, &mut ctx, false, false)
        .expect("the terminator is consumed, then the echo decodes")
        .expect("a per-file index, not NDX_DONE");
    assert_eq!(ndx, 7);
    assert!(ctx.flist_eof, "NDX_FLIST_EOF must set flist_eof");
}

/// Sub-list `dir_ndx` values are NOT monotonic and NOT contiguous.
///
/// WHY: upstream's sender walks its dir_flist while *appending* newly
/// discovered subdirectories to it (`flist.c:2695-2704`), so an `-a` pull of a
/// nested tree interleaves level-1 directories with their children. A measured
/// upstream oracle produced the header sequence `1, 9, 2, 10, 3, 11`. The
/// range check (`rsync.c:361-369` / `flist.c:2622-2626`) is the ONLY constraint:
/// any validation that assumed ordering or contiguity would reject a legitimate
/// upstream stream.
#[test]
fn non_monotonic_dir_ndx_sequence_is_accepted() {
    const OBSERVED: [i32; 6] = [1, 9, 2, 10, 3, 11];

    let dirs: Vec<String> = (0..12).map(|i| format!("d{i}")).collect();
    let dir_refs: Vec<&str> = dirs.iter().map(String::as_str).collect();

    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    for dir_ndx in OBSERVED {
        let entry = format!("d{dir_ndx}/f.txt");
        push_segment(&mut wire, &mut codec, dir_ndx, &[entry.as_str()]);
    }
    codec.write_ndx(&mut wire, NDX_FLIST_EOF).unwrap();

    let mut ctx = inc_recurse_receiver(&dir_refs);
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);

    let mut seen = Vec::new();
    loop {
        match read_ndx_step(&mut reader, &mut read_codec, &mut ctx).expect("segment decodes") {
            NdxStep::Segment(dir_ndx) => seen.push(dir_ndx),
            NdxStep::FlistEof => break,
            other => panic!("unexpected frame {other:?}"),
        }
    }

    assert_eq!(
        seen,
        OBSERVED.to_vec(),
        "every out-of-order dir_ndx must be accepted in arrival order"
    );
    assert_eq!(ctx.file_list().len(), OBSERVED.len());
}

/// `NDX_DEL_STATS` is drained even by a reader whose `inc_recurse` is off.
///
/// WHY: upstream evaluates the `NDX_DEL_STATS` branch (`rsync.c:336-342`)
/// BEFORE the `!inc_recurse || am_sender` gate (`rsync.c:343`). Moving the gate
/// ahead of it would make a plain non-incremental peer abort on a frame it is
/// required to consume, and would desync the five varints that follow.
#[test]
fn del_stats_is_drained_before_the_inc_recurse_gate() {
    let stats = DeleteStats {
        files: 4,
        dirs: 3,
        symlinks: 2,
        devices: 1,
        specials: 5,
    };
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    push_del_stats(&mut wire, &mut codec, &stats);
    codec.write_ndx(&mut wire, NDX_DONE).unwrap();

    let mut sink = NoLazyFlist::new(StreamRole::Receiver, 5);
    assert!(!sink.inc_recurse(), "the gate term under test is off");

    let mut reader = Cursor::new(wire.clone());
    let mut read_codec = create_ndx_codec(PROTOCOL);
    assert_eq!(
        read_ndx_step(&mut reader, &mut read_codec, &mut sink).unwrap(),
        NdxStep::DelStats
    );
    assert_eq!(
        read_ndx_step(&mut reader, &mut read_codec, &mut sink).unwrap(),
        NdxStep::Done
    );
    assert_eq!(
        reader.position() as usize,
        wire.len(),
        "the five deletion varints must be fully consumed"
    );
}

/// The sender drains `NDX_DEL_STATS` too, and hands the counters to its sink.
///
/// WHY: `rsync.c:336-342` runs before `rsync.c:343`, so `am_sender` does not
/// exempt the sender from draining. This is what lets the generator accumulate
/// the receiver's deletion counts during `read_final_goodbye()`
/// (`main.c:904`).
#[test]
fn sender_drains_del_stats_and_receives_the_counters() {
    /// Records every drained frame so the ordering claim is observable.
    struct RecordingSink(Vec<DeleteStats>);

    impl FlistMarkerSink for RecordingSink {
        type FrameMark = ();

        fn role(&self) -> StreamRole {
            StreamRole::Sender
        }

        fn last_file_ndx(&self) -> i32 {
            9
        }

        fn begin_frame(&mut self) {}

        fn on_del_stats(&mut self, stats: &DeleteStats) -> io::Result<()> {
            self.0.push(*stats);
            Ok(())
        }

        // The sender reports INC_RECURSE truthfully; `am_sender` is what
        // rejects markers (see `marker_rejected_by_sender_with_upstream_text`).
        fn inc_recurse(&self) -> bool {
            true
        }
    }

    let stats = DeleteStats {
        files: 7,
        dirs: 1,
        symlinks: 0,
        devices: 0,
        specials: 2,
    };
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    push_del_stats(&mut wire, &mut codec, &stats);
    codec.write_ndx(&mut wire, NDX_DONE).unwrap();

    let mut sink = RecordingSink(Vec::new());
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);

    assert_eq!(
        read_marker_aware_ndx(&mut reader, &mut read_codec, &mut sink).unwrap(),
        NdxFrame::Done
    );
    assert_eq!(sink.0, vec![stats], "the drained counters reach the sink");
}

/// `NDX_DONE` returns without consuming the attribute tail.
///
/// WHY: upstream `rsync.c:334-335` returns from inside the loop, *before* the
/// `read_shortint()` at `rsync.c:383`. A combined read-ndx-and-iflags helper
/// swallows two bytes the peer never sent, which shifts every subsequent frame.
/// The reader position is asserted so the regression is caught even though the
/// returned value would look correct.
#[test]
fn ndx_done_reads_no_attribute_tail() {
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    codec.write_ndx(&mut wire, NDX_DONE).unwrap();
    let done_len = wire.len() as u64;
    // Two bytes that a buggy reader would swallow as `iflags`.
    wire.extend_from_slice(&[0xAB, 0xCD]);

    let mut sink = NoLazyFlist::new(StreamRole::Receiver, 0);
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);

    assert!(
        read_ndx_and_attrs(&mut reader, &mut read_codec, &mut sink, false, false)
            .unwrap()
            .is_none(),
        "NDX_DONE yields no attributes"
    );
    assert_eq!(
        reader.position(),
        done_len,
        "NDX_DONE must not consume the two trailing iflags bytes"
    );
}

/// A per-file index still reads its attribute tail.
///
/// WHY: guards the split in [`SenderAttrs::read_attrs_after_ndx`] against the
/// opposite regression - skipping `rsync.c:383-417` for a real index would
/// leave `iflags` on the wire.
#[test]
fn file_ndx_reads_its_attribute_tail() {
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    push_file_echo(&mut wire, &mut codec, 2, SenderAttrs::ITEM_TRANSFER);
    let total = wire.len() as u64;

    let mut sink = NoLazyFlist::new(StreamRole::Receiver, 9);
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);

    let (ndx, attrs) = read_ndx_and_attrs(&mut reader, &mut read_codec, &mut sink, false, false)
        .unwrap()
        .expect("a per-file index");
    assert_eq!(ndx, 2);
    assert_eq!(attrs.iflags, SenderAttrs::ITEM_TRANSFER);
    assert_eq!(reader.position(), total, "the iflags word must be consumed");
}

/// The sender rejects a file-list marker with upstream's wording.
///
/// WHY: `rsync.c:343-352` - `if (!inc_recurse || am_sender) ... "Invalid file
/// index: %d (%d - %d) [%s]" ... exit_cleanup(RERR_PROTOCOL)`. The sender never
/// consumes sub-list traffic; only the receiver inside the INC_RECURSE window
/// may. The sink below reports `inc_recurse = true`, so a passing test proves
/// the `am_sender` term is what rejects, not the capability term.
#[test]
fn marker_rejected_by_sender_with_upstream_text() {
    /// A sender with INC_RECURSE genuinely negotiated.
    struct IncRecurseSender;

    impl FlistMarkerSink for IncRecurseSender {
        type FrameMark = ();

        fn role(&self) -> StreamRole {
            StreamRole::Sender
        }

        fn last_file_ndx(&self) -> i32 {
            5
        }

        fn begin_frame(&mut self) {}

        fn on_del_stats(&mut self, _stats: &DeleteStats) -> io::Result<()> {
            Ok(())
        }

        fn inc_recurse(&self) -> bool {
            true
        }
    }

    for marker in [NDX_FLIST_EOF, NDX_FLIST_OFFSET] {
        let mut wire = Vec::new();
        let mut codec = create_ndx_codec(PROTOCOL);
        codec.write_ndx(&mut wire, marker).unwrap();

        let mut reader = Cursor::new(wire);
        let mut read_codec = create_ndx_codec(PROTOCOL);
        let err = read_ndx_step(&mut reader, &mut read_codec, &mut IncRecurseSender)
            .expect_err("the sender must reject every file-list marker");

        assert!(
            err.to_string()
                .starts_with(&format!("Invalid file index: {marker} (-1 - 5)")),
            "unexpected message: {err}"
        );
        assert!(
            err.to_string().contains("[sender="),
            "the who_am_i() tag must name the sender: {err}"
        );
        assert!(
            err.get_ref()
                .and_then(|e| e.downcast_ref::<protocol::ProtocolViolation>())
                .is_some(),
            "rejection must map to RERR_PROTOCOL, got {err:?}"
        );
    }
}

/// A receiver outside the INC_RECURSE window rejects markers by the
/// `!inc_recurse` term.
///
/// WHY: the same `rsync.c:343` gate has two terms. This pins the first one, so
/// dropping it would let a phase-boundary or goodbye read silently grow a file
/// list it does not own.
#[test]
fn marker_rejected_by_receiver_without_inc_recurse() {
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    codec.write_ndx(&mut wire, NDX_FLIST_OFFSET).unwrap();

    let mut sink = NoLazyFlist::new(StreamRole::Receiver, 11);
    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);
    let err = read_ndx_step(&mut reader, &mut read_codec, &mut sink)
        .expect_err("a no-lazy-flist sink must reject markers");

    assert!(
        err.to_string()
            .starts_with(&format!("Invalid file index: {NDX_FLIST_OFFSET} (-1 - 11)")),
        "unexpected message: {err}"
    );
    assert!(err.to_string().contains("[receiver="), "message: {err}");
}

/// The receiver's `last_file_ndx` tracks the newest segment, matching upstream's
/// `first_flist->prev->ndx_start + first_flist->prev->used - 1`
/// (`rsync.c:345-348`).
#[test]
fn receiver_last_file_ndx_matches_upstream_span() {
    let ctx = inc_recurse_receiver(&["d0"]);
    // INC_RECURSE starts numbering at 1 (flist.c:2958), so an empty list has no
    // valid index and reports one below the first.
    assert_eq!(FlistMarkerSink::last_file_ndx(&ctx), 0);

    let mut ctx = inc_recurse_receiver(&["d0"]);
    for i in 0..3 {
        ctx.file_list
            .push(FileEntry::new_file(format!("f{i}").into(), 1, 0o100644));
    }
    assert_eq!(FlistMarkerSink::last_file_ndx(&ctx), 3);
}

/// A `ReceiverContext` whose handshake carried no INC_RECURSE flag rejects a
/// sub-list marker by the same `!inc_recurse` term.
///
/// WHY: the pipelined response driver hands its own `ReceiverContext` to
/// [`read_ndx_and_attrs`], so the `rsync.c:343` gate is data-driven there rather
/// than hardcoded by the sink type. Should `inc_recurse()` ever stop consulting
/// the negotiated flags, a peer that never advertised INC_RECURSE could grow the
/// receiver's file list from the transfer stream. This is the negative half of
/// `segment_between_file_echoes_is_consumed_and_never_surfaces`.
#[test]
fn receiver_without_negotiated_inc_recurse_rejects_markers() {
    let mut wire = Vec::new();
    let mut codec = create_ndx_codec(PROTOCOL);
    codec.write_ndx(&mut wire, NDX_FLIST_OFFSET).unwrap();

    let mut ctx = ReceiverContext::new_for_test(&test_handshake(None), test_config());
    assert!(
        !FlistMarkerSink::inc_recurse(&ctx),
        "no INC_RECURSE flag was negotiated"
    );

    let mut reader = Cursor::new(wire);
    let mut read_codec = create_ndx_codec(PROTOCOL);
    let err = read_ndx_step(&mut reader, &mut read_codec, &mut ctx)
        .expect_err("a receiver outside the INC_RECURSE window must reject markers");

    assert!(
        err.to_string()
            .starts_with(&format!("Invalid file index: {NDX_FLIST_OFFSET} (-1 - ")),
        "unexpected message: {err}"
    );
    assert!(err.to_string().contains("[receiver="), "message: {err}");
    assert!(ctx.file_list().is_empty(), "no segment may be absorbed");
}
