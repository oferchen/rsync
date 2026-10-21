//! A local `--fake-super` copy stores a symlink as a placeholder file.
//!
//! Linux cannot set a user xattr on a symlink, so upstream builds there with
//! `NO_SYMLINK_USER_XATTRS` (configure.ac:1466). Its fake-super receiver
//! writes each link as a regular file holding the target, with a `%stat`
//! that records the link type. A real symlink in a fake-super tree has no
//! `%stat`, so its ownership is lost, and a copy back out of the tree no
//! longer matches what upstream reads.
//!
//! # Upstream Reference
//!
//! - `syscall.c:do_symlink_atfd()` - the `am_root < 0` placeholder create.
//! - `syscall.c:do_readlink()` - the `am_root < 0` placeholder read.
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use metadata::FakeSuperStat;
use tempfile::tempdir;

fn user_xattrs_supported(dir: &Path) -> bool {
    let probe = dir.join(".probe");
    fs::write(&probe, b"").expect("write probe");
    let supported = xattr::set(&probe, "user.probe", b"1").is_ok();
    fs::remove_file(&probe).expect("remove probe");
    supported
}

fn copy(source: &Path, dest: &Path, fake_super: bool) {
    let mut source = source.as_os_str().to_owned();
    source.push("/");
    let operands = vec![source, dest.as_os_str().to_owned()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .recursive(true)
        .links(true)
        .owner(true)
        .group(true)
        .permissions(true)
        .fake_super(fake_super);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds");
}

#[test]
fn local_fake_super_copy_round_trips_a_symlink_through_a_placeholder() {
    let temp = tempdir().expect("tempdir");
    if !user_xattrs_supported(temp.path()) {
        eprintln!("skipping: the filesystem rejects user xattrs");
        return;
    }
    let source = temp.path().join("src");
    let tree = temp.path().join("tree");
    let back = temp.path().join("back");
    fs::create_dir(&source).expect("create source");
    std::os::unix::fs::symlink("/nonexistent/target", source.join("link")).expect("create link");
    let owner = fs::symlink_metadata(source.join("link")).expect("lstat link");

    copy(&source, &tree, true);
    let placeholder = tree.join("link");
    let meta = fs::symlink_metadata(&placeholder).expect("lstat placeholder");
    assert!(
        meta.file_type().is_file(),
        "a fake-super link is a regular placeholder, not a {:?}",
        meta.file_type()
    );
    assert_eq!(meta.permissions().mode() & 0o7777, 0o777);
    assert_eq!(
        fs::read(&placeholder).expect("read placeholder"),
        b"/nonexistent/target"
    );
    assert_eq!(
        metadata::load_fake_super(&placeholder).expect("read %stat"),
        Some(FakeSuperStat {
            mode: 0o120777,
            uid: owner.uid(),
            gid: owner.gid(),
            rdev: None,
        })
    );

    // A second copy finds the placeholder up to date and leaves it alone.
    copy(&source, &tree, true);
    assert_eq!(
        fs::read(&placeholder).expect("reread placeholder"),
        b"/nonexistent/target"
    );

    // The fake-super sender reads the target back out of the placeholder.
    copy(&tree, &back, true);
    let restored = back.join("link");
    assert_eq!(
        fs::read(&restored).expect("read copied placeholder"),
        b"/nonexistent/target"
    );
    assert_eq!(
        metadata::load_fake_super(&restored).expect("read copied %stat"),
        metadata::load_fake_super(&placeholder).expect("read %stat")
    );
}
