//! Linux atime-preservation regression test for `--open-noatime`.
//!
//! Drives the `oc-rsync` binary end-to-end through a local copy, with and
//! without `--open-noatime`, and asserts that the source file's access time
//! is preserved only when the flag is set. Mirrors upstream rsync 3.4.2
//! parity for the sender source-file open path.
//!
//! upstream: syscall.c do_open / do_open_nofollow with O_NOATIME
//!
//! The test is Linux-only because `O_NOATIME` is a Linux/Android-specific
//! open flag. It also skips gracefully when:
//! - the host filesystem rejects `O_NOATIME` (some sandbox / overlayfs
//!   setups return `EPERM`, `EINVAL`, `ENOTSUP`, or `EROFS`), or
//! - the host filesystem is mounted with `noatime` / `relatime` such that a
//!   plain read does not advance the on-disk atime (the negative baseline
//!   would never change so the comparison is not meaningful).

#![cfg(target_os = "linux")]

mod integration;

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, SystemTime};

use filetime::{FileTime, set_file_atime};
use integration::helpers::{RsyncCommand, TestDir};

/// Probe whether the filesystem backing `path` honours `O_NOATIME`.
///
/// Some sandboxed filesystems (restricted tmpfs, overlayfs) reject the flag
/// with `EPERM`, `EACCES`, `EINVAL`, `ENOTSUP`, or `EROFS`. Match the same
/// fallback set used by the production helper.
fn filesystem_honours_o_noatime(path: &std::path::Path) -> bool {
    use std::fs::OpenOptions;
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOATIME)
        .open(path)
        .is_ok()
}

/// Read the atime (seconds since epoch) of a file.
fn atime_secs(path: &std::path::Path) -> i64 {
    fs::metadata(path).expect("stat source").atime()
}

#[test]
fn open_noatime_preserves_source_atime_through_cli() {
    let test_dir = TestDir::new().expect("create test dir");
    let src_dir = test_dir.mkdir("src").expect("create src dir");
    let dest_with = test_dir.mkdir("dest_with").expect("create dest_with");
    let dest_without = test_dir.mkdir("dest_without").expect("create dest_without");

    let payload = b"open-noatime regression payload\n";
    let src_with = src_dir.join("with.bin");
    let src_without = src_dir.join("without.bin");
    fs::write(&src_with, payload).expect("write src with");
    fs::write(&src_without, payload).expect("write src without");

    if !filesystem_honours_o_noatime(&src_with) {
        println!("skip: filesystem rejects O_NOATIME on this host");
        return;
    }

    // Backdate atime to one hour ago so a real read would visibly advance it.
    let past = SystemTime::now() - Duration::from_secs(3600);
    let past_ft = FileTime::from_system_time(past);
    set_file_atime(&src_with, past_ft).expect("backdate atime (with)");
    set_file_atime(&src_without, past_ft).expect("backdate atime (without)");

    let baseline_with = atime_secs(&src_with);
    let baseline_without = atime_secs(&src_without);

    // Negative-control copy first: read advances atime when --open-noatime is
    // absent. If the filesystem does not advance atime on read (e.g. mounted
    // `noatime`), the test environment cannot distinguish the two cases.
    let mut without_cmd = RsyncCommand::new();
    without_cmd.args([
        src_without.to_str().expect("utf-8 src"),
        dest_without.to_str().expect("utf-8 dest"),
    ]);
    without_cmd.assert_success();

    let after_without = atime_secs(&src_without);
    if after_without == baseline_without {
        println!("skip: host filesystem does not advance atime on read");
        return;
    }

    // Positive case: --open-noatime must leave the source atime untouched.
    let mut with_cmd = RsyncCommand::new();
    with_cmd.args([
        "--open-noatime",
        src_with.to_str().expect("utf-8 src"),
        dest_with.to_str().expect("utf-8 dest"),
    ]);
    with_cmd.assert_success();

    let after_with = atime_secs(&src_with);
    assert_eq!(
        after_with, baseline_with,
        "--open-noatime must preserve source atime \
         (baseline={baseline_with}, after={after_with})"
    );

    // Sanity: the payload was actually transferred.
    let copied = fs::read(dest_with.join("with.bin")).expect("read dest payload");
    assert_eq!(copied.as_slice(), payload);
}
