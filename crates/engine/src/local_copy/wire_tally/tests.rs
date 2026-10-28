use std::path::PathBuf;

use protocol::flist::FileEntry;
use protocol::wire::SumHead;
use protocol::{CompatibilityFlags, ProtocolVersion};

use super::*;

/// The flags a 3.5.1 local copy negotiates for `-a` with recursion.
fn local_flags() -> CompatibilityFlags {
    CompatibilityFlags::INC_RECURSE
        | CompatibilityFlags::SYMLINK_TIMES
        | CompatibilityFlags::SAFE_FILE_LIST
        | CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | CompatibilityFlags::CHECKSUM_SEED_FIX
        | CompatibilityFlags::INPLACE_PARTIAL_DIR
        | CompatibilityFlags::ID0_NAMES
        | CompatibilityFlags::VARINT_FLIST_FLAGS
}

fn tally(flags: CompatibilityFlags) -> WireTally {
    WireTally::new(
        WireTotals::new(ProtocolVersion::NEWEST, flags),
        &LocalCopyOptions::new(),
        false,
    )
}

fn dir(name: &str) -> FileEntry {
    let mut entry = FileEntry::new_directory(PathBuf::from(name), 0o755);
    entry.set_mtime(1_700_000_000, 0);
    entry
}

fn file(name: &str, size: u64) -> FileEntry {
    let mut entry = FileEntry::new_file(PathBuf::from(name), size, 0o644);
    entry.set_mtime(1_700_000_000, 0);
    entry
}

/// The file-list bytes the same entries cost through the protocol encoder:
/// the initial list, its end marker and `NDX_FLIST_EOF`.
fn flist_bytes(entries: &[FileEntry]) -> u64 {
    let t = tally(local_flags());
    let mut writer = t.flist_writer();
    let mut out = ByteCounter::default();
    writer.set_first_ndx(1);
    for entry in entries {
        writer.write_entry(&mut out, entry).unwrap();
    }
    writer.write_end(&mut out, None).unwrap();
    let mut codec = NdxCodecEnum::new(ProtocolVersion::NEWEST.as_u8());
    codec.write_ndx(&mut out, NDX_FLIST_EOF).unwrap();
    out.0
}

/// A whole-file copy of one 5000-byte file into a fresh destination, the
/// layout 3.5.1 put on the wire for `rsync -a src/ dst/` (frames
/// `[flist] [5046] [00] [0000]` sent, `[22] [00] [000000]` received):
/// - sender reply: `.` echo (1+2), NDX+iflags (1+2), sum head (16), one
///   literal chunk (4+5000), end token (4), whole-file xxh128 (16) = 5046;
/// - generator request: `.` item (3) + NDX+iflags+sum head (19) = 22.
#[test]
fn whole_file_copy_matches_the_upstream_frames() {
    let entries = [dir("."), file("f", 5000)];
    let mut t = tally(local_flags());
    for (i, entry) in entries.iter().enumerate() {
        t.add_entry(entry.clone(), i as u32);
    }
    t.item(Path::new("."));
    t.begin_transfer();
    t.literal(5000);
    t.end_transfer();

    let (sent, received) = t.finish(None, [0; 5]);
    let flist = flist_bytes(&entries);
    assert_eq!(sent, (flist + 4) + (5046 + 4) + (1 + 4) + (2 + 4));
    assert_eq!(received, (22 + 4) + (1 + 4) + (3 + 4));
}

/// Nothing to transfer still costs the file list and the NDX_DONE tail:
/// 3.5.1 sent `[flist] [00] [0000]` and received `[00] [000000]` for an
/// up-to-date tree.
#[test]
fn up_to_date_tree_costs_the_file_list_and_the_tail() {
    let entries = [dir("."), file("f", 5000)];
    let mut t = tally(local_flags());
    for (i, entry) in entries.iter().enumerate() {
        t.add_entry(entry.clone(), i as u32);
    }
    let (sent, received) = t.finish(None, [0; 5]);
    assert_eq!(sent, flist_bytes(&entries) + 4 + 5 + 6);
    assert_eq!(received, 5 + 7);
}

/// A delta transfer: the request carries 4 + s2length bytes per block, the
/// reply one 4-byte token per matched block and one length-prefixed chunk
/// per literal run (token.c:simple_send_token()).
#[test]
fn delta_transfer_counts_block_sums_and_tokens() {
    let entries = [dir("."), file("f", 50_000)];
    let mut t = tally(local_flags());
    for (i, entry) in entries.iter().enumerate() {
        t.add_entry(entry.clone(), i as u32);
    }
    t.begin_transfer();
    t.sum_head(SumHead::with_blocks(72, 700, 2, 300).unwrap());
    for block in 0..28 {
        t.block_match(block, None);
    }
    t.literal(400);
    t.literal(300);
    for block in 29..72 {
        t.block_match(block, None);
    }
    t.end_transfer();
    let (sent, received) = t.finish(None, [0; 5]);
    // 3.5.1 for this layout: reply 1027 bytes, request 451 bytes.
    assert_eq!(sent, flist_bytes(&entries) + 4 + (1027 + 4) + 5 + 6);
    assert_eq!(received, (451 + 4) + 5 + 7);
}

/// A literal run longer than CHUNK_SIZE is split into several
/// length-prefixed chunks, however the engine happened to flush it.
#[test]
fn a_long_literal_run_is_chunked_like_simple_send_token() {
    let mut pending = PendingTransfer {
        entry: 0,
        head: SumHead::WHOLE_FILE,
        tokens: ByteCounter::default(),
        literal_run: 3 * protocol::wire::CHUNK_SIZE as u64 + 1,
    };
    pending.flush_literal_run();
    assert_eq!(pending.tokens.0, 3 * protocol::wire::CHUNK_SIZE as u64 + 1 + 4 * 4);
}

/// `--dry-run` moves no data: the sender only echoes NDX and iflags
/// (sender.c:639-642).
#[test]
fn dry_run_transfer_is_an_echo() {
    let entries = [dir("."), file("f", 5000)];
    let mut t = WireTally::new(
        WireTotals::new(ProtocolVersion::NEWEST, local_flags()),
        &LocalCopyOptions::new(),
        true,
    );
    for (i, entry) in entries.iter().enumerate() {
        t.add_entry(entry.clone(), i as u32);
    }
    t.begin_transfer();
    t.literal(5000);
    t.end_transfer();
    let (sent, received) = t.finish(None, [0; 5]);
    assert_eq!(sent, flist_bytes(&entries) + 4 + (3 + 4) + 5 + 6);
    assert_eq!(received, (3 + 4) + 5 + 7);
}

/// A transfer abandoned before its end token leaves nothing on the wire.
#[test]
fn an_abandoned_transfer_is_not_counted() {
    let entries = [dir("."), file("f", 5000)];
    let mut t = tally(local_flags());
    for (i, entry) in entries.iter().enumerate() {
        t.add_entry(entry.clone(), i as u32);
    }
    t.begin_transfer();
    t.literal(5000);
    t.begin_transfer();
    t.end_transfer();
    let (sent, _) = t.finish(None, [0; 5]);
    assert_eq!(sent, flist_bytes(&entries) + 4 + (1 + 2 + 16 + 4 + 16 + 4) + 5 + 6);
}

/// Each extra sub-list adds one NDX_DONE in each direction
/// (generator.c:2698, sender.c:525-538), carried in the middle tail frame.
#[test]
fn every_sub_list_adds_an_ndx_done_each_way() {
    let flat = [dir("."), file("f", 1)];
    let nested = [dir("."), dir("a"), file("a/g", 1), file("f", 1)];
    let totals = |entries: &[FileEntry]| {
        let mut t = tally(local_flags());
        for (i, entry) in entries.iter().enumerate() {
            t.add_entry(entry.clone(), i as u32);
        }
        t.finish(None, [0; 5])
    };
    let (_, flat_received) = totals(&flat);
    let (_, nested_received) = totals(&nested);
    // One more list: one more NDX_DONE in a new middle frame (1 + 4).
    assert_eq!(nested_received, flat_received + 5);
}

/// Frames split at the 64 KiB output buffer (io.c:1567).
#[test]
fn a_round_beyond_one_buffer_takes_more_frames() {
    assert_eq!(frames::framed_for_test(0), 0);
    assert_eq!(frames::framed_for_test(1), 5);
    assert_eq!(frames::framed_for_test(65_532), 65_536);
    assert_eq!(frames::framed_for_test(65_533), 65_541);
}

/// Unlike incremental lists, a flat list puts owner names in trailing id
/// lists (flist.c:3063) and rides its NDX_DONE in the last request frame.
#[test]
fn flat_list_counts_id_lists_and_merges_the_list_done() {
    let flags = local_flags() & !CompatibilityFlags::INC_RECURSE;
    let entries = [dir("."), file("f", 0)];
    let mut t = tally(flags);
    for (i, entry) in entries.iter().enumerate() {
        t.add_entry(entry.clone(), i as u32);
    }
    let (sent, received) = t.finish(None, [0; 5]);
    // No exchanges: the list's NDX_DONE frame [00] then the phase frame.
    assert_eq!(received, (1 + 4) + (3 + 4));
    assert!(sent > (1 + 4) + (2 + 4));
}

/// `-v` makes the receiver announce the root it creates in one MSG_INFO
/// frame: 3.5.1 received `T2:44` for `created directory /home/ofer/lcsum158/c/dst`.
#[test]
fn the_created_directory_notice_is_one_info_frame() {
    assert_eq!(created_directory_notice_len(Path::new("/home/ofer/lcsum158/c/dst/")), 48);
    assert_eq!(created_directory_notice_len(Path::new("/home/ofer/lcsum158/c/dst")), 48);
    let entries = [dir("."), file("f", 0)];
    let totals = WireTotals::new(ProtocolVersion::NEWEST, local_flags());
    let run = |totals: WireTotals| {
        let mut t = WireTally::new(totals, &LocalCopyOptions::new(), false);
        for (i, entry) in entries.iter().enumerate() {
            t.add_entry(entry.clone(), i as u32);
        }
        t.finish(Some(Path::new("dst/")), [0; 5])
    };
    let (_, quiet) = run(totals);
    let (_, verbose) = run(totals.with_created_directory_notice(true));
    assert_eq!(verbose, quiet + 4 + "created directory dst\n".len() as u64);
}

/// The file list goes out in `readdir()` order (flist.c:send_directory()),
/// and prefix compression makes its size depend on that order: here the
/// readdir order shares no prefix between neighbours while sorted order
/// shares `a`.
#[test]
fn the_file_list_is_encoded_in_readdir_order() {
    let entries = [dir("."), file("ab", 1), file("ac", 1), file("b", 1)];
    let sent_for = |readdir: [u32; 4]| {
        let mut t = tally(local_flags());
        for (entry, pos) in entries.iter().zip(readdir) {
            t.add_entry(entry.clone(), pos);
        }
        t.finish(None, [0; 5]).0
    };
    let sorted = sent_for([0, 0, 1, 2]);
    let interleaved = sent_for([0, 0, 2, 1]);
    let wire = |order: [usize; 4]| {
        let wire: Vec<FileEntry> = order.iter().map(|&i| entries[i].clone()).collect();
        flist_bytes(&wire)
    };
    assert_eq!(sorted - wire([0, 1, 2, 3]), interleaved - wire([0, 1, 3, 2]));
    assert!(interleaved > sorted, "the interleaved order loses the shared prefix");
}
