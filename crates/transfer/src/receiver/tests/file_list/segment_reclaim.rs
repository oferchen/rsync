//! `reclaim_oldest_segment`: an INC_RECURSE receiver frees the entries of its
//! oldest finished segment and never the last one.

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

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
