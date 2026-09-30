//! A send-time I/O error must reach the receiver's late delete pass.
//!
//! The sender reports an unreadable source file through `MSG_IO_ERROR`, which
//! it writes only after its send loop ends, just before its final `NDX_DONE`
//! (upstream: sender.c:811-820). Upstream's generator runs the late deletions
//! only after the receiver has read that `NDX_DONE`: generator.c:2891-2900
//! waits for the receiver's third `MSG_DONE`, which main.c:1111 sends after
//! `recv_files()` returns. By then io.c:1740-1747 has folded the error into
//! `io_error`, so `--delete-after`'s `delete_in_dir()` skips its deletions
//! (generator.c:304-311). A receiver that sweeps before reading the sender's
//! final `NDX_DONE` misses the error and deletes destination files whose
//! source may simply have gone unlisted.
//!
//! `--delete-delay` and `--delete-during` decide during the walk, before the
//! error exists, so both still delete (generator.c:265-278
//! `do_delayed_deletions()` replays the recorded victims with no `io_error`
//! test). The three modes together pin both the guard and its scope. Verified
//! against upstream 3.5.1 over a loopback rsh, pushing and pulling: delay and
//! during delete the extra file, after keeps it and prints the notice, and
//! every run exits 23.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

const NOTICE: &str = "IO error encountered -- skipping file deletion";

/// Builds `root/src/d` with a readable and an unreadable file, and
/// `root/dst/d` holding an extra file the source does not list. Returns false
/// when the running user can still read a mode-000 file, since then the
/// sender raises no error.
fn fixture(root: &Path) -> bool {
    let src = root.join("src/d");
    let dst = root.join("dst/d");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::write(src.join("keep"), b"keep\n").expect("write keep");
    let unreadable = src.join("unreadable");
    fs::write(&unreadable, b"secret\n").expect("write unreadable");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).expect("chmod 000");
    fs::write(dst.join("extra"), b"stale\n").expect("write extra");
    fs::read(&unreadable).is_err()
}

fn run(root: &Path, mode: &str, push: bool) -> test_support::CliOutput {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let src = format!("{}/", root.join("src").display());
    let dst = format!("{}/", root.join("dst").display());
    let (src, dst) = if push {
        (src, format!("localhost:{dst}"))
    } else {
        (format!("localhost:{src}"), dst)
    };
    OcRsyncCliRunner::new()
        .arg("-r")
        .arg(format!("--{mode}"))
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(src)
        .arg(dst)
        .run()
        .expect("transfer run")
}

fn check(push: bool) {
    for (mode, deletes) in [
        ("delete-delay", true),
        ("delete-during", true),
        ("delete-after", false),
    ] {
        let tmp = create_tempdir();
        if !fixture(tmp.path()) {
            eprintln!("skipping: a mode-000 file is readable by this user");
            return;
        }
        let out = run(tmp.path(), mode, push);
        out.assert_exit(23);
        let extra = tmp.path().join("dst/d/extra");
        assert_eq!(
            !extra.exists(),
            deletes,
            "{mode} (push={push}): extra deleted must be {deletes}\n{}{}",
            out.stdout_str(),
            out.stderr_str()
        );
        // On a push the notice is printed by the remote receiver, which oc
        // does not relay to the client yet, so only the pull checks it.
        if !push {
            let notices = format!("{}{}", out.stdout_str(), out.stderr_str())
                .matches(NOTICE)
                .count();
            assert_eq!(notices, usize::from(!deletes), "{mode}: skip notice count");
        }
        assert!(
            tmp.path().join("dst/d/keep").exists(),
            "{mode}: keep arrives"
        );
    }
}

#[test]
fn push_late_delete_honours_send_time_io_error() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    check(true);
}

#[test]
fn pull_late_delete_honours_send_time_io_error() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    check(false);
}
