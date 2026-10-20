//! `--fake-super` local copies of a source that already carries a recorded
//! `user.rsync.%stat`, compared against upstream rsync 3.5.1.
//!
//! Upstream reads the source through `x_lstat()`, so the recorded stat - not
//! the placeholder inode - is the file's mode/uid/gid. The receiver then runs
//! `set_stat_xattr()`: the real mode becomes the recorded permission bits with
//! owner access forced on, and `%stat` is written only when the real
//! mode/owner/group cannot represent the recorded values.
//!
//! # Upstream Reference
//!
//! - `xattrs.c:1216-1266` `get_stat_xattr()` (via `x_lstat()`)
//! - `xattrs.c:1304-1356` `set_stat_xattr()`
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use metadata::{FAKE_SUPER_XATTR, FakeSuperStat};
use tempfile::tempdir;

fn read_stat(path: &Path) -> Option<FakeSuperStat> {
    let raw = xattr::get(path, FAKE_SUPER_XATTR).ok().flatten()?;
    FakeSuperStat::decode(std::str::from_utf8(&raw).ok()?).ok()
}

/// Writes `value` as the source's recorded stat, or returns `false` when the
/// filesystem has no `user.*` xattr support so the caller can skip loudly.
fn record_stat(path: &Path, value: &str) -> bool {
    match xattr::set(path, FAKE_SUPER_XATTR, value.as_bytes()) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "skipping: user xattrs unsupported on {}: {error}",
                path.display()
            );
            false
        }
    }
}

/// `rsync -a --fake-super`: owner, group, perms, times and recursion.
fn copy_archive(source: &Path, dest: &Path) {
    let operands = vec![source.as_os_str().to_owned(), dest.as_os_str().to_owned()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .owner(true)
        .group(true)
        .permissions(true)
        .times(true)
        .fake_super(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");
}

/// A recorded `100600 0,0 77:77` on a 0644 placeholder: upstream sets the real
/// mode to 0600 (`(fmode & ACCESSPERMS) | 0600`, xattrs.c:1314-1315) and keeps
/// the `%stat`, because uid/gid 77 differ from the copying user.
#[test]
fn recorded_stat_sets_the_real_mode_and_keeps_the_foreign_owner() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("mkdir src");
    let file = source.join("withstat");
    fs::write(&file, b"a\n").expect("write");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
    if !record_stat(&file, "100600 0,0 77:77") {
        return;
    }

    copy_archive(&source, &dest);

    let copied = dest.join("src").join("withstat");
    let mode = fs::metadata(&copied).expect("stat dest").mode() & 0o7777;
    assert_eq!(
        mode, 0o600,
        "real mode must follow the recorded 0600, as upstream"
    );
    let stat = read_stat(&copied).expect("foreign owner must keep a %stat");
    assert_eq!(stat.mode, 0o100600);
    assert_eq!((stat.uid, stat.gid), (77, 77));
}

/// A recorded stat that equals the copy's real stat is redundant: upstream
/// leaves no `%stat` on the destination (xattrs.c:1325-1336).
#[test]
fn recorded_stat_equal_to_the_real_stat_is_not_stored() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("mkdir src");
    let file = source.join("same");
    fs::write(&file, b"a\n").expect("write");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
    let meta = fs::metadata(&file).expect("stat src");
    let recorded = format!("100644 0,0 {}:{}", meta.uid(), meta.gid());
    if !record_stat(&file, &recorded) {
        return;
    }

    copy_archive(&source, &dest);

    let copied = dest.join("src").join("same");
    let mode = fs::metadata(&copied).expect("stat dest").mode() & 0o7777;
    assert_eq!(mode, 0o644);
    assert!(
        read_stat(&copied).is_none(),
        "a %stat equal to the real stat must not be stored, as upstream"
    );
}

/// Placeholders for a FIFO, a socket, a char and a block device whose recorded owner is
/// the copying user. The recorded type differs from the regular placeholder on
/// disk, so upstream keeps every `%stat` (xattrs.c:1314-1326 compares the
/// placeholder's own `S_IFMT` against the recorded mode) and the copy still
/// reads back as the special file.
#[test]
fn recorded_special_types_survive_a_same_owner_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("mkdir src");
    let cases = [
        ("fifo", 0o010644, "0,0", None),
        ("socket", 0o140644, "0,0", None),
        ("char", 0o020644, "41,67", Some((41, 67))),
        ("block", 0o060644, "42,69", Some((42, 69))),
    ];
    for (name, mode, rdev, _) in cases {
        let file = source.join(name);
        fs::write(&file, b"").expect("write placeholder");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod");
        let meta = fs::metadata(&file).expect("stat src");
        let recorded = format!("{mode:o} {rdev} {}:{}", meta.uid(), meta.gid());
        if !record_stat(&file, &recorded) {
            return;
        }
    }

    copy_archive(&source, &dest);

    for (name, mode, _, rdev) in cases {
        let copied = dest.join("src").join(name);
        assert!(
            fs::symlink_metadata(&copied).expect("stat dest").is_file(),
            "{name}: the placeholder stays a regular file"
        );
        let stat = read_stat(&copied).unwrap_or_else(|| panic!("{name}: %stat must be kept"));
        assert_eq!(stat.mode, mode, "{name}: recorded type and perms");
        assert_eq!(stat.rdev, rdev, "{name}: recorded device numbers");
    }
}
