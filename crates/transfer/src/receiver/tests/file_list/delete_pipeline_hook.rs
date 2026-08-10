//! DDP-B3 (#2257) integration: the receiver's per-segment hook publishes
//! one [`engine::delete::DeletePlan`] into the shared map per INC_RECURSE
//! segment and updates the emitter-side traversal cursor with the
//! segment's child directories.

use std::path::PathBuf;

use protocol::flist::FileEntry;

use super::super::super::ReceiverContext;
use super::super::support::{test_config, test_handshake};

/// DDP-B3 (#2257) integration check: a synthetic INC_RECURSE state with
/// a `DeleteContext` attached publishes one [`engine::delete::DeletePlan`]
/// per segment into the shared [`engine::delete::DeletePlanMap`], and the
/// emitter-side traversal cursor records the segment's child directories.
#[test]
fn delete_pipeline_hook_publishes_one_plan_per_segment() {
    use std::sync::Arc;

    use engine::delete::{DeleteContext, DeletePlanMap};

    // Build a destination tree with extras the receiver should plan to
    // delete:
    //   <root>/sub1/keep
    //   <root>/sub1/extra
    //   <root>/sub2/keep
    //   <root>/sub2/extra
    let tmp = tempfile::TempDir::new().unwrap();
    for sub in ["sub1", "sub2"] {
        let dir = tmp.path().join(sub);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("keep"), b"").unwrap();
        std::fs::write(dir.join("extra"), b"").unwrap();
    }

    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Build a flist matching what the receiver would have after the
    // initial segment plus two INC_RECURSE segments. Segment table is
    // laid out so wire NDX 1 -> "sub1", wire NDX 2 -> "sub2".
    ctx.file_list = vec![
        FileEntry::new_directory(PathBuf::from("sub1"), 0o755),
        FileEntry::new_directory(PathBuf::from("sub2"), 0o755),
        // sub1 segment entries (flat 2..=3)
        FileEntry::new_file(PathBuf::from("keep"), 0, 0o644),
        FileEntry::new_directory(PathBuf::from("nested1"), 0o755),
        // sub2 segment entries (flat 4..=5)
        FileEntry::new_file(PathBuf::from("keep"), 0, 0o644),
        FileEntry::new_directory(PathBuf::from("nested2"), 0o755),
    ];
    // Initial segment owns wire 1..=2 at flat 0..=1; segments owning
    // wire 4..=5 at flat 2..=3, then 7..=8 at flat 4..=5.
    ctx.ndx_segments = vec![(0, 1), (2, 4), (4, 7)];

    let map = Arc::new(DeletePlanMap::new());
    let delete_ctx = Arc::new(DeleteContext::with_shared_plan_map(
        Arc::clone(&map),
        tmp.path().to_path_buf(),
        true,
    ));
    ctx.set_delete_context(Some(Arc::clone(&delete_ctx)));

    // Observe the synthetic root segment first so the traversal cursor
    // knows sub1 and sub2 are children of the root. The receiver
    // normally does this implicitly when `receive_file_list` lands the
    // initial flist; here we feed it directly so the cursor walk in
    // the assertion below can descend into both subtrees.
    delete_ctx
        .observe_segment_for_delete(
            std::path::Path::new(""),
            &[
                FileEntry::new_directory(PathBuf::from("sub1"), 0o755),
                FileEntry::new_directory(PathBuf::from("sub2"), 0o755),
            ],
        )
        .expect("root observe ok");

    // dir_ndx wire 1 -> "sub1" segment at flat_start 2
    ctx.publish_segment_to_delete_pipeline(1, 2);
    // dir_ndx wire 2 -> "sub2" segment at flat_start 4
    ctx.publish_segment_to_delete_pipeline(2, 4);

    assert_eq!(
        map.len(),
        3,
        "root + two segments -> three plans (root, sub1, sub2)"
    );
    // Drop the root plan so the rest of the assertions focus on sub1
    // and sub2 alone.
    let _ = map.take(std::path::Path::new(""));
    let sub1_plan = map.take(std::path::Path::new("sub1")).expect("sub1 plan");
    let sub2_plan = map.take(std::path::Path::new("sub2")).expect("sub2 plan");

    let sub1_names: Vec<&std::ffi::OsStr> = sub1_plan
        .extras
        .iter()
        .map(|e| e.name.as_os_str())
        .collect();
    let sub2_names: Vec<&std::ffi::OsStr> = sub2_plan
        .extras
        .iter()
        .map(|e| e.name.as_os_str())
        .collect();
    assert_eq!(sub1_names, vec![std::ffi::OsStr::new("extra")]);
    assert_eq!(sub2_names, vec![std::ffi::OsStr::new("extra")]);

    // Cursor should have learned about nested1 + nested2 as child dirs.
    // `cursor_snapshot` drains the producer channel, applies the
    // observations to a private cursor, and re-enqueues them so the
    // eventual drain still sees the full set.
    let cursor_lock = delete_ctx.cursor_snapshot();
    let mut cursor = cursor_lock.lock().unwrap();
    let seq: Vec<PathBuf> = std::iter::from_fn(|| cursor.next_ready()).collect();
    assert!(seq.contains(&PathBuf::from("sub1/nested1")));
    assert!(seq.contains(&PathBuf::from("sub2/nested2")));
}

/// DDP-B3 (#2257): with no [`engine::delete::DeleteContext`] attached,
/// the segment hook is a no-op even when invoked directly.
#[test]
fn delete_pipeline_hook_is_noop_when_no_context_attached() {
    let handshake = test_handshake();
    let config = test_config();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = vec![FileEntry::new_directory(PathBuf::from("sub"), 0o755)];
    ctx.ndx_segments = vec![(0, 1)];

    // Should not panic, should not touch any external state. The
    // absence of a DeleteContext means publish is a pure return.
    ctx.publish_segment_to_delete_pipeline(1, 1);
    assert!(ctx.delete_context().is_none());
}
