//! A push into an oc server receiver under INC_RECURSE must survive the
//! hard-link follower rows it forwards mid-walk.
//!
//! A server receiver itemizes every new hard-link follower to the client
//! sender (`NDX + iflags + xname`), and the sender echoes each one back
//! (upstream sender.c:584-602). Under INC_RECURSE the receiver also writes a
//! per-sub-list `NDX_DONE` mid-walk and reads its echo (generator.c:2219-2239,
//! sender.c:525-539). Both echoes share one ordered stream, so a follower echo
//! left unread ahead of the `NDX_DONE` echo, or ahead of the next sub-list's
//! transfer replies, desyncs the receiver and the push dies with exit 12 before
//! the follower is linked.
//!
//! The fixture mirrors upstream's `testsuite/hardlinks_test.py`: the leader
//! `name1` sits in the top-level sub-list and one follower lives three
//! directories down, so its sub-list is walked after the top-level one has
//! been released.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

fn hardlink_fixture(src: &Path) {
    let deep = src.join("subdir/down/deep");
    fs::create_dir_all(&deep).expect("mkdir deep");
    fs::write(src.join("name1"), b"This is the file\n").expect("write name1");
    fs::hard_link(src.join("name1"), src.join("name2")).expect("link name2");
    fs::hard_link(src.join("name1"), src.join("name3")).expect("link name3");
    fs::write(src.join("name4"), b"This is the file\n").expect("write name4");
    for name in ["aa", "ab"] {
        fs::write(src.join("subdir").join(name), b"").expect("write subdir file");
    }
    fs::hard_link(src.join("name1"), deep.join("new-file")).expect("link new-file");
}

#[test]
fn inc_recurse_push_links_a_follower_whose_leader_is_in_an_earlier_sub_list() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let src = tmp.path().join("from");
    let dest = tmp.path().join("to");
    hardlink_fixture(&src);
    fs::create_dir_all(&dest).expect("mkdir dest");

    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let out = OcRsyncCliRunner::new()
        .arg("-aHi")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("{}/", src.display()))
        .arg(format!("localhost:{}/", dest.display()))
        .run()
        .expect("push run");
    out.assert_success();

    let leader = fs::metadata(dest.join("name1")).expect("name1 delivered");
    for name in ["name2", "name3", "subdir/down/deep/new-file"] {
        let meta = fs::metadata(dest.join(name)).expect("link delivered");
        assert_eq!(meta.ino(), leader.ino(), "{name} must share name1's inode");
    }

    // upstream: hlink.c:300-490 itemizes each follower once, while the
    // generator walks the sub-list that holds it. Four names share one inode,
    // so exactly three distinct rows name a follower (which name leads is the
    // sender's choice).
    let rows = out.stdout_str();
    let followers: Vec<&str> = rows
        .lines()
        .filter_map(|line| line.strip_prefix("hf+++++++++ "))
        .filter_map(|row| row.split(" => ").next())
        .collect();
    let distinct: BTreeSet<&str> = followers.iter().copied().collect();
    assert_eq!(
        (followers.len(), distinct.len()),
        (3, 3),
        "each follower must be itemized exactly once, got:\n{rows}"
    );
}
