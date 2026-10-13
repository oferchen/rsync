//! A push into an oc server receiver under INC_RECURSE must send each
//! sub-list's symlink and special-file itemize rows with that sub-list.
//!
//! A server receiver forwards an itemize row (`NDX + iflags`) for an entry it
//! creates without a transfer, such as a new FIFO (upstream generator.c:582-593,
//! sender.c:295-297). Upstream emits it inline while recv_generator() walks the
//! sub-list, so the row reaches the sender before the sub-list's `NDX_DONE`
//! frees it (generator.c:2219-2239). A row held back past that point names an
//! entry whose list is gone, and the push dies.
//!
//! The fixture puts a FIFO in `a/` and three empty directories under it. Each
//! empty directory is its own empty sub-list, so `a/`'s list is released
//! before the next sub-list with a file (`b/`) is walked.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::Path;
use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};
fn fifo_fixture(src: &Path) {
    let a = src.join("a");
    for dir in ["e1", "e2", "e3"] {
        fs::create_dir_all(a.join(dir)).expect("mkdir empty sub-list");
    }
    fs::create_dir_all(src.join("b")).expect("mkdir b");
    fs::write(a.join("f"), b"one\n").expect("write a/f");
    fs::write(src.join("b/f"), b"two\n").expect("write b/f");
    let status = std::process::Command::new("mkfifo")
        .arg(a.join("p"))
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed");
}
#[test]
fn inc_recurse_push_itemizes_a_fifo_before_its_sub_list_is_released() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let src = tmp.path().join("from");
    let dest = tmp.path().join("to");
    fifo_fixture(&src);
    fs::create_dir_all(&dest).expect("mkdir dest");
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let out = OcRsyncCliRunner::new()
        .arg("-aDi")
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
    let meta = fs::symlink_metadata(dest.join("a/p")).expect("a/p delivered");
    assert!(meta.file_type().is_fifo(), "a/p must arrive as a FIFO");
    assert_eq!(fs::read(dest.join("b/f")).expect("b/f delivered"), b"two\n");
    // upstream 3.5.1 prints the row once, inside a/'s sub-list: after a/f and
    // before the empty directories that follow it.
    let rows = out.stdout_str();
    let lines: Vec<&str> = rows.lines().collect();
    let pos = |row: &str| lines.iter().position(|l| *l == row);
    let fifo = pos("cS+++++++++ a/p");
    assert_eq!(
        rows.matches("cS+++++++++ a/p").count(),
        1,
        "the FIFO must be itemized exactly once, got:\n{rows}"
    );
    assert!(
        fifo < pos("cd+++++++++ a/e1/"),
        "the FIFO row must precede the next sub-list, got:\n{rows}"
    );
}
