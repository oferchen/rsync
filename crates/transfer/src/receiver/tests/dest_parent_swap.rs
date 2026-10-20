//! Receiver directory passes under a destination parent swapped after entry.
//!
//! Upstream resolves every per-entry operation against the destination it
//! entered once (`main.c` `get_local_name()` -> `change_dir()`), through the
//! held parent descriptor or a beneath-only walk (`syscall.c:3858`
//! `held_dfd_for()`, `util1.c:189-240` `make_path()`). A directory inside the
//! destination replaced by a symlink to a path outside it must therefore never
//! carry a create, chmod or utimes out of the tree.
//!
//! Each test opens the sandbox on the destination, then swaps `dest/a` for a
//! symlink to `outside/` and runs one receiver pass. The bait under `outside/`
//! must be untouched.
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

use filetime::FileTime;
use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::{TestDeletionWriter, test_handshake};
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

fn receiver(entries: Vec<FileEntry>) -> ReceiverContext {
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-lptDre.".to_owned(),
        flags: ParsedServerFlags {
            links: true,
            perms: true,
            times: true,
            devices: true,
            specials: true,
            recursive: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), config);
    ctx.file_list = entries.into();
    ctx
}

/// Opens the sandbox on `dest/`, then replaces `dest/a` with a symlink to
/// `outside/`. Returns the sandbox and the `outside/` path.
fn sandbox_then_swap(root: &Path) -> (fast_io::DirSandbox, std::path::PathBuf) {
    let dest = root.join("dest");
    let outside = root.join("outside");
    fs::create_dir_all(dest.join("a")).unwrap();
    fs::create_dir(&outside).unwrap();
    let sandbox = fast_io::DirSandbox::open_root(&dest).unwrap();
    fs::remove_dir(dest.join("a")).unwrap();
    symlink(&outside, dest.join("a")).unwrap();
    (sandbox, outside)
}

#[test]
fn a_special_under_a_swapped_parent_creates_nothing_outside() {
    let tmp = tempfile::tempdir().unwrap();
    let (sandbox, outside) = sandbox_then_swap(tmp.path());
    let ctx = receiver(vec![FileEntry::new_fifo("a/x/pipe".into(), 0o640)]);

    let _ = ctx.create_specials_in_range(
        0..1,
        &tmp.path().join("dest"),
        None,
        None,
        Some(&sandbox),
        &mut TestDeletionWriter,
    );

    assert!(
        fs::symlink_metadata(outside.join("x")).is_err(),
        "make_path() for the special's parent followed the swapped symlink"
    );
}

#[test]
fn a_symlink_under_a_swapped_parent_creates_nothing_outside() {
    let tmp = tempfile::tempdir().unwrap();
    let (sandbox, outside) = sandbox_then_swap(tmp.path());
    let ctx = receiver(vec![FileEntry::new_symlink(
        "a/x/link".into(),
        0o777,
        "target".into(),
    )]);

    let _ = ctx.create_symlinks_in_range(
        0..1,
        &tmp.path().join("dest"),
        Some(&sandbox),
        &mut TestDeletionWriter,
    );

    assert!(
        fs::symlink_metadata(outside.join("x")).is_err(),
        "make_path() for the symlink's parent followed the swapped symlink"
    );
}

/// `touch_up_dirs()` re-applies a read-only mode and the sender's mtime to
/// `a/b`; with `a` swapped, neither may reach `outside/b`.
#[test]
fn touch_up_dirs_never_retouches_a_directory_outside_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let (sandbox, outside) = sandbox_then_swap(tmp.path());
    let bait = outside.join("b");
    fs::create_dir(&bait).unwrap();
    fs::set_permissions(&bait, fs::Permissions::from_mode(0o755)).unwrap();
    let bait_mtime = FileTime::from_unix_time(5_000_000, 0);
    filetime::set_file_mtime(&bait, bait_mtime).unwrap();

    let mut dir = FileEntry::new_directory("a/b".into(), 0o555);
    dir.set_mtime(1_000_000, 0);
    let ctx = receiver(vec![dir]);

    ctx.touch_up_dirs(
        &tmp.path().join("dest"),
        Some(&sandbox),
        &mut TestDeletionWriter,
    );

    let meta = fs::metadata(&bait).unwrap();
    assert_eq!(
        FileTime::from_last_modification_time(&meta),
        bait_mtime,
        "touch_up_dirs() re-timed a directory outside the destination"
    );
    assert_eq!(
        meta.permissions().mode() & 0o777,
        0o755,
        "touch_up_dirs() chmodded a directory outside the destination"
    );
}

/// Non-vacuity companion: the same pass through an intact `a/` does retouch
/// `a/b`, so the refusal above is a real defence and not a pass that never
/// acts.
#[test]
fn touch_up_dirs_retouches_a_nested_directory_inside_the_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("dest");
    fs::create_dir_all(dest.join("a/b")).unwrap();
    let sandbox = fast_io::DirSandbox::open_root(&dest).unwrap();

    let mut dir = FileEntry::new_directory("a/b".into(), 0o555);
    dir.set_mtime(1_000_000, 0);
    let ctx = receiver(vec![dir]);

    ctx.touch_up_dirs(&dest, Some(&sandbox), &mut TestDeletionWriter);

    let meta = fs::metadata(dest.join("a/b")).unwrap();
    assert_eq!(
        FileTime::from_last_modification_time(&meta),
        FileTime::from_unix_time(1_000_000, 0)
    );
    if !metadata::am_root() {
        assert_eq!(meta.permissions().mode() & 0o777, 0o555);
    }
    fs::set_permissions(dest.join("a/b"), fs::Permissions::from_mode(0o755)).unwrap();
}
