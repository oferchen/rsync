//! Where the operator's trust ends and the transfer's confinement begins.
//!
//! Upstream enters the destination operand with a plain `change_dir()`
//! (`main.c` `get_local_name()`), so a symlink the operator put anywhere in
//! that path is followed. Each entry's parent is then resolved relative to the
//! cwd through `secure_relative_open(NULL, dirpath, ...)` (`syscall.c:1245`
//! `do_lchown_at()`, likewise `do_chmod_at()` and the utimes wrapper), so a
//! symlink BELOW the root is confined.
//!
//! Both halves are pinned here against one fixture - `base/link -> real`
//! above the root `base/link/inner`:
//!
//! - The operator's symlink must be followed for the root entry and the names
//!   under it. Walking the fused absolute path refuses it, which is what
//!   aborted a local copy into such a destination with exit 23.
//! - A symlink planted inside the root that points outside must still be
//!   refused for chmod, utimes and chown, so trusting the operator's prefix
//!   cannot be widened into following the transfer's own names.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use filetime::{FileTime, set_file_times};
use metadata::{
    DestinationRoot, MetadataError, MetadataOptions, apply_directory_metadata_with_options,
    apply_file_metadata_with_options,
};

/// 2001-09-09T01:46:40Z - the mtime the source carries.
const SOURCE_MTIME: FileTime = FileTime::from_unix_time(1_000_000_000, 0);
/// 1993-03-01T00:00:00Z - the outside sentinel's mtime, distinct from the source.
const WITNESS_MTIME: FileTime = FileTime::from_unix_time(730_000_000, 0);

struct Layout {
    _tmp: tempfile::TempDir,
    /// `base/link/inner` - the operator's destination operand.
    root: PathBuf,
    /// `base/real/inner` - where the operator's path physically resolves.
    real_root: PathBuf,
    /// A file outside the destination entirely.
    outside_sentinel: PathBuf,
    source: fs::Metadata,
}

fn layout() -> Layout {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = tmp.path().join("base");
    let real_root = base.join("real/inner");
    fs::create_dir_all(&real_root).expect("mkdir real/inner");
    symlink("real", base.join("link")).expect("operator symlink");

    let outside = tmp.path().join("outside");
    fs::create_dir(&outside).expect("mkdir outside");
    let outside_sentinel = outside.join("sentinel");
    fs::write(&outside_sentinel, b"OUTSIDE").expect("write sentinel");
    fs::set_permissions(&outside_sentinel, fs::Permissions::from_mode(0o600))
        .expect("sentinel mode");
    set_file_times(&outside_sentinel, WITNESS_MTIME, WITNESS_MTIME).expect("sentinel mtime");

    let source_path = tmp.path().join("source");
    fs::write(&source_path, b"src").expect("write source");
    fs::set_permissions(&source_path, fs::Permissions::from_mode(0o751)).expect("source mode");
    set_file_times(&source_path, SOURCE_MTIME, SOURCE_MTIME).expect("source mtime");
    let source = fs::metadata(&source_path).expect("stat source");

    Layout {
        _tmp: tmp,
        root: base.join("link/inner"),
        real_root,
        outside_sentinel,
        source,
    }
}

/// `-p -t` plus a chown to the caller's own ids, so all three path-based
/// appliers run and none needs privilege.
fn options(root: &Path, own: &fs::Metadata) -> MetadataOptions {
    MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .with_owner_override(Some(own.uid()))
        .with_group_override(Some(own.gid()))
        .with_destination_root(Some(Arc::new(DestinationRoot::new(root.to_path_buf()))))
}

fn errno_of(err: &MetadataError) -> Option<i32> {
    std::error::Error::source(err)
        .and_then(|e| e.downcast_ref::<std::io::Error>())
        .and_then(std::io::Error::raw_os_error)
}

#[test]
fn operator_symlink_above_the_root_is_followed() {
    let l = layout();
    fs::create_dir(l.real_root.join("sub")).expect("mkdir sub");
    fs::write(l.real_root.join("sub/file"), b"dst").expect("write dest");
    let own = fs::metadata(l.real_root.join("sub/file")).expect("stat dest");
    let opts = options(&l.root, &own);

    apply_file_metadata_with_options(&l.root.join("sub/file"), &l.source, &opts)
        .expect("a name below the root resolves through the operator's symlink");
    apply_directory_metadata_with_options(&l.root, &l.source, opts, None)
        .expect("the root entry itself resolves through the operator's symlink");

    for landed in [l.real_root.join("sub/file"), l.real_root.clone()] {
        let meta = fs::metadata(&landed).expect("stat landed");
        assert_eq!(
            FileTime::from_last_modification_time(&meta),
            SOURCE_MTIME,
            "{landed:?} mtime"
        );
        assert_eq!(meta.mode() & 0o777, 0o751, "{landed:?} mode");
    }
}

#[test]
fn symlink_below_the_root_is_still_refused() {
    let l = layout();
    // The transfer's own name `inner/sub` swapped for a symlink leading out.
    symlink(
        l.outside_sentinel.parent().expect("outside dir"),
        l.real_root.join("sub"),
    )
    .expect("plant symlink below root");
    let own = fs::metadata(&l.outside_sentinel).expect("stat sentinel");
    let attack = l.root.join("sub/sentinel");

    // Each applier alone, so one refusing first cannot mask another following.
    let chmod_only = MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(false)
        .with_destination_root(Some(Arc::new(DestinationRoot::new(l.root.clone()))));
    let times_only = MetadataOptions::new()
        .preserve_permissions(false)
        .preserve_times(true)
        .with_destination_root(Some(Arc::new(DestinationRoot::new(l.root.clone()))));
    let chown_only = options(&l.root, &own)
        .preserve_permissions(false)
        .preserve_times(false);

    for (what, opts) in [
        ("chmod", chmod_only),
        ("utimes", times_only),
        ("chown", chown_only),
    ] {
        let err = apply_file_metadata_with_options(&attack, &l.source, &opts)
            .expect_err(&format!("{what} must not follow a symlink below the root"));
        assert!(
            matches!(
                errno_of(&err),
                Some(libc::ELOOP | libc::ENOTDIR | libc::EXDEV)
            ),
            "{what}: expected a refused parent open, got {err}"
        );
    }

    let after = fs::metadata(&l.outside_sentinel).expect("stat sentinel");
    assert_eq!(after.mode() & 0o7777, 0o600, "outside mode untouched");
    assert_eq!(
        FileTime::from_last_modification_time(&after),
        WITNESS_MTIME,
        "outside mtime untouched"
    );
}
