//! An rsh SERVER SENDER must not follow a source directory swapped for an
//! escaping symlink after the file list was built.
//!
//! The sender scans the tree first and opens file content later, so a parent
//! directory replaced in between redirects a by-path open outside the tree.
//! upstream: `rsync-3.5.1/flist.c:2967-2969` holds each explicit source root
//! by dev/ino, and `sender.c:694-704` opens content beneath that held root, so
//! the escaping component is refused and the file fails with exit 23.
//!
//! The swap is made deterministic with a per-directory merge file (`-F`) that
//! is a FIFO in a directory scanned after the target: the sender blocks
//! opening it mid-scan, the test swaps the already-listed directory, then
//! releases the FIFO so the scan completes and the content opens run.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// Bounds every wait so a sender that never reaches the FIFO fails the test.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(60);
const SECRET: &[u8] = b"outside-secret\n";

/// Builds `root/src/a/f`, `root/src/zz/.rsync-filter` (a FIFO) and the
/// escape target `root/outside/f`.
fn fixture(root: &Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("a")).expect("mkdir a");
    fs::create_dir_all(src.join("zz")).expect("mkdir zz");
    fs::create_dir_all(root.join("outside")).expect("mkdir outside");
    fs::write(src.join("a/f"), b"in-tree\n").expect("write a/f");
    fs::write(root.join("outside/f"), SECRET).expect("write outside/f");
    let fifo = src.join("zz/.rsync-filter");
    let status = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo");
    assert!(status.success(), "mkfifo failed");
    fifo
}

/// Pulls `src/` through the lsh stub with `-F`, on its own thread.
fn spawn_pull(root: &Path, sandbox_off: bool) -> mpsc::Receiver<CliOutput> {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let mut runner = OcRsyncCliRunner::new()
        .arg("-aF")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", root.join("src").display()))
        .arg(format!("{}/", root.join("dst").display()))
        .timeout(HANDSHAKE_TIMEOUT);
    if sandbox_off {
        runner = runner
            .env("OC_RSYNC_NO_LANDLOCK", "1")
            .env("OC_RSYNC_NO_SECCOMP", "1");
    }
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(runner.run().expect("pull run"));
    });
    rx
}

/// Waits until the sender has opened the FIFO for reading, which proves the
/// scan has already listed `a`, then swaps `a` for an escaping symlink and
/// releases the FIFO.
fn swap_mid_scan(root: &Path, fifo: PathBuf) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(fs::OpenOptions::new().write(true).open(fifo));
    });
    let writer = rx
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("the sender never opened the merge-file FIFO")
        .expect("open FIFO for writing");
    let src = root.join("src");
    fs::rename(src.join("a"), src.join("a.moved")).expect("move a aside");
    symlink("../outside", src.join("a")).expect("escaping symlink");
    drop(writer);
}

fn pull_with_swap(sandbox_off: bool) {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let fifo = fixture(tmp.path());
    let done = spawn_pull(tmp.path(), sandbox_off);
    swap_mid_scan(tmp.path(), fifo);
    let out = done
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect("pull did not finish");

    let leaked = fs::read(tmp.path().join("dst/a/f")).ok();
    assert_ne!(
        leaked.as_deref(),
        Some(SECRET),
        "the sender followed the swapped directory out of the source tree\nstderr:\n{}",
        out.stderr_str()
    );
    out.assert_exit(23);
}

#[test]
fn rsh_sender_refuses_a_directory_swapped_after_the_scan() {
    pull_with_swap(false);
}

/// The same refusal must come from the sender's own open, not from the
/// kernel sandbox layers.
#[test]
fn rsh_sender_refuses_a_swapped_directory_without_the_kernel_sandbox() {
    pull_with_swap(true);
}

/// `from/sym-to-dir/..` stats as `other` but cleans to `from`, so the held
/// root and the scanned directory disagree. rsync 3.5.1 refuses the scan with
/// ELOOP and exits 23 with nothing transferred (3.5.0 sent `other`).
/// upstream: `rsync-3.5.1/flist.c:302-305` records the root, `flist.c:2265-2266`
/// opens the cleaned operand through it, `flist.c:325-329` refuses the changed
/// identity and `flist.c:2374` reports the per-item `opendir` failure.
#[test]
fn rsh_sender_refuses_a_parent_dir_operand_through_a_symlinked_component() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let base = tmp.path().join("base");
    fs::create_dir_all(base.join("from")).expect("mkdir from");
    fs::create_dir_all(base.join("other/dir_target")).expect("mkdir dir_target");
    fs::write(base.join("other/dir_target/c.txt"), b"CCC").expect("write c.txt");
    symlink("../other/dir_target", base.join("from/sym-to-dir")).expect("plant sym-to-dir");
    let dst = tmp.path().join("dst");
    fs::create_dir(&dst).expect("mkdir dst");
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let out = OcRsyncCliRunner::new()
        .arg("-a")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!(
            "localhost:{}",
            base.join("from/sym-to-dir/..").display()
        ))
        .arg(format!("{}/", dst.display()))
        .timeout(HANDSHAKE_TIMEOUT)
        .run()
        .expect("pull run");
    assert!(
        out.stderr_str()
            .contains("Too many levels of symbolic links"),
        "the scan must fail with ELOOP\nstderr:\n{}",
        out.stderr_str()
    );
    out.assert_exit(23);
    let left: Vec<_> = fs::read_dir(&dst).expect("read dst").collect();
    assert!(left.is_empty(), "nothing may be transferred: {left:?}");
}
