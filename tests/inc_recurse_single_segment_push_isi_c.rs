//! ISI.c - single-segment INC_RECURSE sender interop.
//!
//! An rsync 3.5.2 client pulls a ten-file flat tree from an oc-rsync
//! `--server --sender` reached through a remote shell, and then from a
//! 3.5.2 sender on the same source. The client negotiates protocol 32 with
//! INC_RECURSE, so the oc sender runs its incremental file list against a
//! real upstream receiver. With ten files the whole list is one segment
//! (flist.c:46 `MIN_FILECNT_LOOKAHEAD` is 1000), so the end-of-list marker
//! follows the first segment directly.
//!
//! Both pulls must exit 0, land the same tree as the source, and print the
//! same itemized rows on the client.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use std::fs;

use core::client::ClientConfig;
use integration::isi_harness::{peers, pin_mtimes, pull_both, snapshot, write_fixture_file};
use test_support::{UpstreamVersion, require_upstream_rsync};
use transfer::setup::build_capability_string;

const SIZES: [usize; 10] = [0, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];

/// Default `ClientConfig` must advertise INC_RECURSE unconditionally.
#[test]
fn default_capability_string_includes_inc_recurse() {
    let config = ClientConfig::builder().build();
    assert!(
        config.inc_recursive_send(),
        "inc_recursive_send default must be ON"
    );
    let caps = build_capability_string(config.inc_recursive_send());
    assert!(
        caps.starts_with("-e."),
        "capability string must keep the -e. prefix: {caps}"
    );
    assert!(
        caps.contains('i'),
        "capability string must include 'i' by default: {caps}"
    );
}

/// `--no-inc-recursive` must still suppress `'i'` even with the default ON,
/// as upstream's `set_allow_inc_recurse()` lets the option win.
#[test]
fn no_inc_recursive_override_suppresses_inc_recurse_bit() {
    let config = ClientConfig::builder().inc_recursive_send(false).build();
    assert!(!config.inc_recursive_send());
    let caps = build_capability_string(config.inc_recursive_send());
    assert!(
        !caps.contains('i'),
        "--no-inc-recursive must drop 'i': {caps}"
    );
}

#[test]
fn single_segment_pull_from_oc_sender_matches_rsync_3_5_2() {
    let Some(peers) = peers(require_upstream_rsync(UpstreamVersion::V3_5_2)) else {
        return;
    };
    let work = tempfile::tempdir().expect("work dir");
    let src = work.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    for (idx, size) in SIZES.iter().enumerate() {
        write_fixture_file(&src.join(format!("file_{idx:02}.bin")), idx as u32, *size);
    }
    pin_mtimes(&src);
    let source = snapshot(&src);
    assert_eq!(source.len(), SIZES.len());

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
