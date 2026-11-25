//! ISI.d - multi-segment INC_RECURSE sender interop.
//!
//! As ISI.c, with a nested tree (`a/b/c`, `a/b/d`, `a/e/f`, ten files in
//! each leaf). Under INC_RECURSE the sender sends each directory's contents
//! as its own sub-list (flist.c `send_extra_file_list()`), so the oc sender
//! must interleave several sub-lists with transfers before the end-of-list
//! marker, and an rsync 3.5.2 receiver must accept them.
//!
//! Both pulls must exit 0, land the same tree as the source, and print the
//! same itemized rows on the client.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use std::fs;

use integration::isi_harness::{Entry, peers, pin_mtimes, pull_both, snapshot, write_fixture_file};
use test_support::{UpstreamVersion, require_upstream_rsync};

const LEAVES: [&str; 3] = ["a/b/c", "a/b/d", "a/e/f"];
const SIZES: [usize; 10] = [0, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];

#[test]
fn multi_segment_pull_from_oc_sender_matches_rsync_3_5_2() {
    let Some(peers) = peers(require_upstream_rsync(UpstreamVersion::V3_5_2)) else {
        return;
    };
    let work = tempfile::tempdir().expect("work dir");
    let src = work.path().join("src");
    for (dir_idx, leaf) in LEAVES.iter().enumerate() {
        let dir = src.join(leaf);
        fs::create_dir_all(&dir).expect("create leaf");
        for (idx, size) in SIZES.iter().enumerate() {
            let seed = (dir_idx * SIZES.len() + idx) as u32;
            write_fixture_file(&dir.join(format!("file_{idx:02}.bin")), seed, *size);
        }
    }
    pin_mtimes(&src);
    let source = snapshot(&src);
    let dirs = source.values().filter(|e| **e == Entry::Dir).count();
    assert_eq!(dirs, 6, "a, a/b, a/b/c, a/b/d, a/e, a/e/f");
    assert_eq!(source.len(), 6 + LEAVES.len() * SIZES.len());

    let ((oc, oc_tree), (up, up_tree)) = pull_both(&peers, work.path(), &src, &["-rti"]);
    assert_eq!(up.code, Some(0), "3.5.2 sender: {}", up.stderr);
    assert_eq!(oc.code, Some(0), "oc sender: {}", oc.stderr);
    assert_eq!(up_tree, source, "3.5.2 sender must land the source tree");
    assert_eq!(oc_tree, source, "oc sender must land the source tree");
    assert_eq!(
        oc.stdout, up.stdout,
        "itemized rows, oc sender vs 3.5.2 sender"
    );
}
