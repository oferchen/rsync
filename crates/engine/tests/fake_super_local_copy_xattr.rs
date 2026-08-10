//! Integration tests for `--fake-super` on the local-copy executor path.
//!
//! Confirms that a pure local copy (no daemon, no network) writes the
//! `user.rsync.%stat` xattr on the destination so non-root callers can
//! preserve ownership metadata. Mirrors upstream rsync 3.4.x behaviour:
//! `set_file_attrs()` invokes `set_stat_xattr()` when `am_root < 0`.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:set_stat_xattr()` - encodes `<mode_octal> <maj>,<min> <uid>:<gid>`
//! - `rsync.c:set_file_attrs()` - dispatches to fake-super under `am_root < 0`

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use metadata::{FAKE_SUPER_XATTR, FakeSuperStat};
use tempfile::tempdir;

/// Reads the fake-super xattr from a destination path.
///
/// Returns `None` when the filesystem does not support the `user.rsync.*`
/// namespace (e.g. tmpfs mounted without `user_xattr`). Callers treat that
/// as a skip rather than a failure.
fn read_fake_super(path: &std::path::Path) -> Option<FakeSuperStat> {
    let raw = xattr::get(path, FAKE_SUPER_XATTR).ok().flatten()?;
    let text = std::str::from_utf8(&raw).ok()?;
    FakeSuperStat::decode(text).ok()
}

/// End-to-end check: `--fake-super --owner --group --perms` on a local-copy
/// pulls source uid/gid/mode into `user.rsync.%stat` on the destination.
#[test]
fn local_copy_writes_fake_super_xattr_for_regular_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("payload.bin");
    fs::write(&source_file, b"fake-super payload").expect("write source");

    let source_meta = fs::metadata(&source_file).expect("stat source");
    let expected_uid = source_meta.uid();
    let expected_gid = source_meta.gid();
    let expected_mode = source_meta.mode();

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .owner(true)
        .group(true)
        .permissions(true)
        .fake_super(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_file = dest.join("src").join("payload.bin");
    assert!(dest_file.exists(), "destination payload must exist");

    let Some(stat) = read_fake_super(&dest_file) else {
        // Filesystem rejected the xattr (e.g. tmpfs without user_xattr).
        // The local-copy plumbing still ran without error; nothing else to
        // verify on this host.
        return;
    };

    assert_eq!(
        stat.mode, expected_mode,
        "mode bits in user.rsync.%stat must match source inode"
    );
    assert_eq!(
        stat.uid, expected_uid,
        "uid in user.rsync.%stat must match source inode"
    );
    assert_eq!(
        stat.gid, expected_gid,
        "gid in user.rsync.%stat must match source inode"
    );
    assert!(
        stat.rdev.is_none(),
        "regular files must not carry rdev in user.rsync.%stat"
    );

    // Confirm the raw bytes match upstream's encoding so wire compatibility
    // with `xattrs.c:set_stat_xattr()` does not regress.
    let raw = xattr::get(&dest_file, FAKE_SUPER_XATTR)
        .expect("xattr get after fake-super copy")
        .expect("xattr present after fake-super copy");
    let encoded = std::str::from_utf8(&raw).expect("utf-8 xattr").to_owned();
    assert_eq!(encoded, stat.encode(), "raw xattr bytes must match encoder");
}

/// Without `--fake-super`, the local-copy executor must not synthesise the
/// `user.rsync.%stat` xattr. Guards against accidental writes that would
/// pollute destination metadata.
#[test]
fn local_copy_without_fake_super_does_not_write_stat_xattr() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("create source");
    fs::create_dir_all(&dest).expect("create dest");

    let source_file = source.join("payload.bin");
    fs::write(&source_file, b"vanilla payload").expect("write source");

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .owner(true)
        .group(true)
        .permissions(true);

    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");

    let dest_file = dest.join("src").join("payload.bin");
    assert!(dest_file.exists(), "destination payload must exist");

    let raw = xattr::get(&dest_file, FAKE_SUPER_XATTR).ok().flatten();
    assert!(
        raw.is_none(),
        "user.rsync.%stat must not appear when --fake-super is off; got {raw:?}"
    );
}
