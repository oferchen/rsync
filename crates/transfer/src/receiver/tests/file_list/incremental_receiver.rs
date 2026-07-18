//! `IncrementalFileListReceiver::try_read_one` coverage: pending vs ready
//! buffering, parent-before-child ordering, mark-finished, drain, and
//! interleaving with `next_ready` / `drain_ready` / `collect_sorted`.

use std::io::Cursor;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// Helper: create wire-encoded file list data from entries.
fn encode_entries(entries: &[FileEntry]) -> Vec<u8> {
    let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);

    for entry in entries {
        writer.write_entry(&mut data, entry).unwrap();
    }
    writer.write_end(&mut data, None).unwrap();

    data
}

/// Helper: create an `IncrementalFileListReceiver` from raw wire data.
fn make_receiver(
    data: Vec<u8>,
) -> super::super::super::IncrementalFileListReceiver<Cursor<Vec<u8>>> {
    let handshake = test_handshake();
    let config = test_config();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.incremental_file_list_receiver(Cursor::new(data))
}

#[test]
fn try_read_one_returns_false_when_finished() {
    // Create a receiver that's already marked as finished
    let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
    let flist_reader = protocol::flist::FileListReader::new(protocol);

    // Empty data - will hit EOF immediately
    let empty_data: Vec<u8> = vec![0]; // Single zero byte = end of list marker
    let source = Cursor::new(empty_data);

    let incremental = protocol::flist::IncrementalFileList::new();

    let mut receiver = super::super::super::IncrementalFileListReceiver {
        flist_reader,
        source,
        incremental,
        finished_reading: true, // Already finished
        entries_read: 0,
        use_qsort: false,
        iconv_reorder_suppressed: false,
    };

    assert!(!receiver.try_read_one().unwrap());
}

#[test]
fn try_read_one_on_empty_list_returns_false() {
    // An empty file list (only the end-of-list marker) should
    // cause try_read_one to hit EOF and return false.
    let data = encode_entries(&[]);
    let mut receiver = make_receiver(data);

    assert!(!receiver.try_read_one().unwrap());
    assert!(receiver.is_finished_reading());
    assert_eq!(receiver.entries_read(), 0);
}

#[test]
fn try_read_one_reads_single_entry() {
    let file = FileEntry::new_file("hello.txt".into(), 42, 0o644);
    let data = encode_entries(&[file]);
    let mut receiver = make_receiver(data);

    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 1);
    assert_eq!(receiver.ready_count(), 1);
    assert!(!receiver.is_finished_reading());

    let entry = receiver.next_ready().unwrap().unwrap();
    assert_eq!(entry.name(), "hello.txt");
    assert_eq!(entry.size(), 42);
}

#[test]
fn try_read_one_reads_entries_one_at_a_time() {
    let entries = vec![
        FileEntry::new_file("a.txt".into(), 10, 0o644),
        FileEntry::new_file("b.txt".into(), 20, 0o644),
        FileEntry::new_file("c.txt".into(), 30, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 1);
    assert_eq!(receiver.ready_count(), 1);

    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 2);
    assert_eq!(receiver.ready_count(), 2);

    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 3);
    assert_eq!(receiver.ready_count(), 3);

    assert!(!receiver.try_read_one().unwrap());
    assert!(receiver.is_finished_reading());

    let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
        .map(|e| e.name().to_string())
        .collect();
    assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
}

#[test]
fn try_read_one_after_eof_is_idempotent() {
    let data = encode_entries(&[FileEntry::new_file("only.txt".into(), 1, 0o644)]);
    let mut receiver = make_receiver(data);

    assert!(receiver.try_read_one().unwrap());
    // EOF, then subsequent calls stay false.
    assert!(!receiver.try_read_one().unwrap());
    assert!(!receiver.try_read_one().unwrap());
    assert!(!receiver.try_read_one().unwrap());
    assert!(receiver.is_finished_reading());
}

#[test]
fn try_read_one_child_before_parent_stays_pending() {
    // Child file arrives before its parent directory.
    // try_read_one should add it to pending, not ready.
    let entries = vec![
        FileEntry::new_file("subdir/child.txt".into(), 100, 0o644),
        FileEntry::new_directory("subdir".into(), 0o755),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read child first - goes to pending since "subdir" doesn't exist
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 1);
    assert_eq!(receiver.ready_count(), 0);
    assert_eq!(receiver.pending_count(), 1);

    // Read parent directory - should release child too
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 2);
    assert_eq!(receiver.ready_count(), 2); // dir + file
    assert_eq!(receiver.pending_count(), 0);
}

#[test]
fn try_read_one_with_pre_marked_directory() {
    // Mark a directory as created before reading. A child entry
    // should become immediately ready.
    let entries = vec![FileEntry::new_file("existing/file.txt".into(), 50, 0o644)];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    receiver.mark_directory_created("existing");

    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 1);
    assert_eq!(receiver.pending_count(), 0);

    let entry = receiver.next_ready().unwrap().unwrap();
    assert_eq!(entry.name(), "existing/file.txt");
}

#[test]
fn try_read_one_deeply_nested_out_of_order() {
    // Push entries in reverse depth order, then verify resolution.
    let entries = vec![
        FileEntry::new_file("a/b/c/deep.txt".into(), 1, 0o644),
        FileEntry::new_directory("a/b/c".into(), 0o755),
        FileEntry::new_directory("a/b".into(), 0o755),
        FileEntry::new_directory("a".into(), 0o755),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read deep file - pending (no ancestors)
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 0);
    assert_eq!(receiver.pending_count(), 1);

    // Read "a/b/c" - pending (parent "a/b" missing)
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 0);
    assert_eq!(receiver.pending_count(), 2);

    // Read "a/b" - pending (parent "a" missing)
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 0);
    assert_eq!(receiver.pending_count(), 3);

    // Read "a" - cascading release: a -> a/b -> a/b/c -> deep.txt
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 4);
    assert_eq!(receiver.pending_count(), 0);
}

#[test]
fn try_read_one_interleaved_with_next_ready() {
    let entries = vec![
        FileEntry::new_file("first.txt".into(), 1, 0o644),
        FileEntry::new_file("second.txt".into(), 2, 0o644),
        FileEntry::new_file("third.txt".into(), 3, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read one, consume it, read next
    assert!(receiver.try_read_one().unwrap());
    let e1 = receiver.next_ready().unwrap().unwrap();
    assert_eq!(e1.name(), "first.txt");
    assert_eq!(receiver.ready_count(), 0);

    assert!(receiver.try_read_one().unwrap());
    let e2 = receiver.next_ready().unwrap().unwrap();
    assert_eq!(e2.name(), "second.txt");

    assert!(receiver.try_read_one().unwrap());
    let e3 = receiver.next_ready().unwrap().unwrap();
    assert_eq!(e3.name(), "third.txt");

    // No more
    assert!(!receiver.try_read_one().unwrap());
    assert!(receiver.next_ready().unwrap().is_none());
}

#[test]
fn try_read_one_interleaved_with_drain_ready() {
    let entries = vec![
        FileEntry::new_file("x.txt".into(), 1, 0o644),
        FileEntry::new_file("y.txt".into(), 2, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read both entries
    assert!(receiver.try_read_one().unwrap());
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 2);

    // Drain all at once
    let drained = receiver.drain_ready();
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].name(), "x.txt");
    assert_eq!(drained[1].name(), "y.txt");
    assert_eq!(receiver.ready_count(), 0);

    // EOF
    assert!(!receiver.try_read_one().unwrap());
}

#[test]
fn try_read_one_directory_and_children() {
    let entries = vec![
        FileEntry::new_directory("mydir".into(), 0o755),
        FileEntry::new_file("mydir/alpha.txt".into(), 10, 0o644),
        FileEntry::new_file("mydir/beta.txt".into(), 20, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read directory
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 1);

    // Read children - they should be immediately ready since parent exists
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 2);

    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.ready_count(), 3);

    // Verify order
    let names: Vec<String> = std::iter::from_fn(|| receiver.next_ready().ok().flatten())
        .map(|e| e.name().to_string())
        .collect();
    assert_eq!(names, vec!["mydir", "mydir/alpha.txt", "mydir/beta.txt"]);
}

#[test]
fn try_read_one_is_empty_tracks_state_correctly() {
    let entries = vec![FileEntry::new_file("f.txt".into(), 1, 0o644)];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Not empty initially (haven't read yet, not finished)
    assert!(!receiver.is_finished_reading());

    // Read the entry
    assert!(receiver.try_read_one().unwrap());
    // Not empty: still has a ready entry
    assert!(!receiver.is_empty());

    // Hit EOF
    assert!(!receiver.try_read_one().unwrap());
    // Still not empty: one ready entry remains
    assert!(!receiver.is_empty());

    // Consume the entry
    receiver.next_ready().unwrap();
    // Now truly empty
    assert!(receiver.is_empty());
}

#[test]
fn try_read_one_reads_symlink_entry() {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.links = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);

    // Encode a symlink entry with links preserved
    let protocol = protocol::ProtocolVersion::try_from(32u8).unwrap();
    let mut data = Vec::new();
    let mut writer = protocol::flist::FileListWriter::new(protocol);
    writer = writer.with_preserve_links(true);

    let symlink = FileEntry::new_symlink("link.txt".into(), "/target".into());
    writer.write_entry(&mut data, &symlink).unwrap();
    writer.write_end(&mut data, None).unwrap();

    let mut receiver = ctx.incremental_file_list_receiver(Cursor::new(data));

    assert!(receiver.try_read_one().unwrap());
    let entry = receiver.next_ready().unwrap().unwrap();
    assert!(entry.is_symlink());
    assert_eq!(entry.name(), "link.txt");
}

#[test]
fn try_read_one_increments_entries_read() {
    let entries = vec![
        FileEntry::new_file("one.txt".into(), 1, 0o644),
        FileEntry::new_file("two.txt".into(), 2, 0o644),
        FileEntry::new_file("three.txt".into(), 3, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    assert_eq!(receiver.entries_read(), 0);

    receiver.try_read_one().unwrap();
    assert_eq!(receiver.entries_read(), 1);

    receiver.try_read_one().unwrap();
    assert_eq!(receiver.entries_read(), 2);

    receiver.try_read_one().unwrap();
    assert_eq!(receiver.entries_read(), 3);

    // EOF does not increment
    receiver.try_read_one().unwrap();
    assert_eq!(receiver.entries_read(), 3);
}

#[test]
fn try_read_one_partial_then_collect_sorted() {
    let entries = vec![
        FileEntry::new_file("z.txt".into(), 1, 0o644),
        FileEntry::new_file("a.txt".into(), 2, 0o644),
        FileEntry::new_file("m.txt".into(), 3, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read one entry via try_read_one
    assert!(receiver.try_read_one().unwrap());
    // Consume it so it doesn't appear in collect_sorted's drain
    let first = receiver.next_ready().unwrap().unwrap();
    assert_eq!(first.name(), "z.txt");

    // Now collect the remaining entries sorted
    let sorted = receiver.collect_sorted().unwrap();
    assert_eq!(sorted.len(), 2);
    // "a.txt" should come before "m.txt" after sorting
    assert_eq!(sorted[0].name(), "a.txt");
    assert_eq!(sorted[1].name(), "m.txt");
}

#[test]
fn mark_finished_prevents_further_reads() {
    let entries = vec![
        FileEntry::new_file("a.txt".into(), 1, 0o644),
        FileEntry::new_file("b.txt".into(), 2, 0o644),
    ];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    // Read one entry
    assert!(receiver.try_read_one().unwrap());
    assert_eq!(receiver.entries_read(), 1);

    // Mark as finished (simulating error recovery)
    receiver.mark_finished();

    // try_read_one should now return false even though data remains
    assert!(!receiver.try_read_one().unwrap());
    assert!(receiver.is_finished_reading());
    assert_eq!(receiver.entries_read(), 1);
}

#[test]
fn try_read_one_stats_are_accessible() {
    let entries = vec![FileEntry::new_file("stat_test.txt".into(), 999, 0o644)];
    let data = encode_entries(&entries);
    let mut receiver = make_receiver(data);

    assert!(receiver.try_read_one().unwrap());
    // Stats should reflect one regular file read
    let stats = receiver.stats();
    assert_eq!(stats.num_files, 1);
    assert_eq!(stats.total_size, 999);
}

#[test]
fn receiver_reclaim_oldest_segment_frees_entries() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Manually populate the file list with 6 entries across 3 segments.
    for i in 0..6 {
        ctx.file_list.push(FileEntry::new_file(
            format!("file_{i}.txt").into(),
            (i + 1) as u64 * 100,
            0o644,
        ));
    }
    // Segments: [0..2), [2..4), [4..6)
    ctx.ndx_segments = vec![(0, 1), (2, 4), (4, 7)];
    ctx.first_segment_idx = 0;

    // Reclaim first segment.
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.first_segment_idx, 1);
    assert_eq!(ctx.file_list[0].name(), ""); // reclaimed
    assert_eq!(ctx.file_list[1].name(), ""); // reclaimed
    assert_eq!(ctx.file_list[2].name(), "file_2.txt"); // intact
    assert_eq!(ctx.file_list[4].name(), "file_4.txt"); // intact

    // Reclaim second segment.
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.first_segment_idx, 2);
    assert_eq!(ctx.file_list[2].name(), ""); // reclaimed
    assert_eq!(ctx.file_list[3].name(), ""); // reclaimed
    assert_eq!(ctx.file_list[4].name(), "file_4.txt"); // intact

    // Third reclaim is a no-op (last segment).
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.first_segment_idx, 2); // unchanged
    assert_eq!(ctx.file_list[4].name(), "file_4.txt"); // intact
}

#[test]
fn receiver_reclaim_noop_with_single_segment() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    ctx.file_list
        .push(FileEntry::new_file("f.txt".into(), 100, 0o644));
    // Single segment - no reclamation possible.
    ctx.reclaim_oldest_segment();
    assert_eq!(ctx.first_segment_idx, 0);
    assert_eq!(ctx.file_list[0].name(), "f.txt");
}
