//! The macOS clonefile fast path must leave the destination exactly as the
//! portable copy path would: a fresh `open()`ed file that carries only the
//! metadata the transfer preserves.
//!
//! `clonefile(2)` copies the source's xattrs (resource fork and FinderInfo
//! included) and timestamps verbatim, and inherits the target directory's
//! ACLs. Each clone test first asserts the clone path really ran, so a silent fall
//! back to a data copy cannot make an assertion pass.
#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use apple_fs::{
    FINDER_INFO_LEN, read_finder_info, read_resource_fork, write_finder_info, write_resource_fork,
};
use engine::local_copy::clonefile::{CloneResult, clone_or_copy};
use engine::local_copy::{
    FilterProgram, FilterProgramEntry, LocalCopyExecution, LocalCopyOptions, LocalCopyPlan,
    LocalCopySummary,
};
use fast_io::{CopyMethod, CopyResult, PlatformCopy};
use filters::FilterRule;
use tempfile::tempdir;

const CLONEFILE_LABEL: &str = "clonefile (CoW)";

fn apfs_clonefile_supported(dir: &Path) -> bool {
    let probe_src = dir.join(".probe_src");
    let probe_dst = dir.join(".probe_dst");
    if fs::write(&probe_src, b"probe").is_err() {
        return false;
    }
    let cloned = matches!(
        clone_or_copy(&probe_src, &probe_dst),
        Ok(CloneResult::Cloned)
    );
    let _ = fs::remove_file(&probe_src);
    let _ = fs::remove_file(&probe_dst);
    cloned
}

fn copy_file(source: &Path, dest: &Path, options: LocalCopyOptions) -> LocalCopySummary {
    let operands = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest.as_os_str()),
    ];
    LocalCopyPlan::from_operands(&operands)
        .expect("plan")
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("local copy succeeds")
}

/// Copies `source` to `dest` and asserts the clonefile fast path took it.
fn clone_file(source: &Path, dest: &Path, options: LocalCopyOptions) {
    let summary = copy_file(source, dest, options);
    assert_eq!(
        summary.copy_method_breakdown(),
        vec![(CLONEFILE_LABEL, 1)],
        "the copy must take the clonefile fast path"
    );
}

fn crtime_secs(path: &Path) -> i64 {
    fs::metadata(path)
        .expect("stat")
        .created()
        .expect("crtime")
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("after epoch")
        .as_secs() as i64
}

#[test]
fn test_clonefile_crtimes_normalized_when_crtimes_off() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_crtime.txt");
    let dest_off = temp.path().join("dst_crtime_off.txt");
    let dest_on = temp.path().join("dst_crtime_on.txt");
    fs::write(&source, b"creation time test content").expect("write source");

    let historical_secs: i64 = 1_600_000_000;
    metadata::set_crtime(&source, historical_secs).expect("set source crtime");
    assert_eq!(crtime_secs(&source), historical_secs);

    let before_copy = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("duration")
        .as_secs() as i64;

    clone_file(
        &source,
        &dest_off,
        LocalCopyOptions::new().crtimes(false).times(true),
    );
    let dest_off_crtime = crtime_secs(&dest_off);
    assert!(
        dest_off_crtime >= before_copy - 5,
        "creation time when --crtimes is off should be current time ({dest_off_crtime} >= {before_copy} - 5)"
    );

    clone_file(
        &source,
        &dest_on,
        LocalCopyOptions::new().crtimes(true).times(true),
    );
    assert_eq!(
        crtime_secs(&dest_on),
        historical_secs,
        "destination creation time must match source when --crtimes is on"
    );
}

/// Without `-A` the clone keeps only what an `open()`ed file inherits from its
/// directory; with `-A` finalize applies the source ACL.
///
/// The no-`-A` assertion expects an empty ACL, which holds only while the
/// temp dir has no inheritable entries; that fixture assumption is asserted
/// up front so a host with an inheriting temp dir fails loudly.
#[cfg(feature = "acl")]
#[test]
fn test_clonefile_acls_follow_the_acl_option() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }
    let dir_acl = exacl::getfacl(temp.path(), None).expect("getfacl temp dir");
    assert!(
        dir_acl.is_empty(),
        "fixture assumes a temp dir with no inheritable ACL entries: {dir_acl:?}"
    );

    let source = temp.path().join("src_acl.txt");
    let dest_no_acl = temp.path().join("dst_no_acl.txt");
    let dest_with_acl = temp.path().join("dst_with_acl.txt");
    fs::write(&source, b"acl clone content").expect("write source");

    let acl_entries = vec![exacl::AclEntry::allow_user(
        "nobody",
        exacl::Perm::READ,
        exacl::Flag::empty(),
    )];
    if let Err(err) = exacl::setfacl(&[&source], &acl_entries, None) {
        eprintln!("setfacl not supported or permission denied: {err}, skipping ACL test");
        return;
    }
    assert!(
        !exacl::getfacl(&source, None)
            .expect("getfacl source")
            .is_empty(),
        "source must have extended ACL entries"
    );

    clone_file(&source, &dest_no_acl, LocalCopyOptions::new().acls(false));
    let dst_acl = exacl::getfacl(&dest_no_acl, None).expect("getfacl dest no acl");
    assert!(
        dst_acl.is_empty(),
        "a clone without -A must not carry the source ACL: {dst_acl:?}"
    );

    clone_file(&source, &dest_with_acl, LocalCopyOptions::new().acls(true));
    assert!(
        !exacl::getfacl(&dest_with_acl, None)
            .expect("getfacl dest with acl")
            .is_empty(),
        "a clone with -A must carry the source ACL"
    );
}

#[test]
fn test_clonefile_resource_fork_and_finder_info_preservation_when_xattrs_on() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_rf.txt");
    let dest = temp.path().join("dst_rf.txt");
    fs::write(&source, b"resource fork test file data").expect("write source");
    let rf_payload = b"macOS ResourceFork binary test data 0123456789".to_vec();
    write_resource_fork(&source, &rf_payload).expect("write resource fork");
    let finder_info = [0x5au8; FINDER_INFO_LEN];
    write_finder_info(&source, &finder_info).expect("write finder info");

    clone_file(&source, &dest, LocalCopyOptions::new().xattrs(true));

    assert_eq!(
        read_resource_fork(&dest).expect("read dest resource fork"),
        Some(rf_payload),
        "ResourceFork must be preserved exactly when -X is on"
    );
    assert_eq!(
        read_finder_info(&dest).expect("read dest finder info"),
        Some(finder_info),
        "FinderInfo must be preserved exactly when -X is on"
    );
}

#[test]
fn test_clonefile_large_resource_fork_preservation() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_large_rf.txt");
    let dest = temp.path().join("dst_large_rf.txt");
    fs::write(&source, b"large resource fork data").expect("write source");
    let large_rf = vec![0xa5u8; 2048];
    write_resource_fork(&source, &large_rf).expect("write large resource fork");

    clone_file(&source, &dest, LocalCopyOptions::new().xattrs(true));

    assert_eq!(
        read_resource_fork(&dest).expect("read dest large resource fork"),
        Some(large_rf)
    );
}

#[test]
fn test_clonefile_resource_fork_and_finder_info_stripped_when_xattrs_off() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_strip_rf.txt");
    let dest = temp.path().join("dst_strip_rf.txt");
    fs::write(&source, b"resource fork stripping test data").expect("write source");
    write_resource_fork(&source, b"macOS ResourceFork to be stripped").expect("write rf");
    write_finder_info(&source, &[0x33u8; FINDER_INFO_LEN]).expect("write finder info");

    clone_file(&source, &dest, LocalCopyOptions::new().xattrs(false));

    assert_eq!(
        read_resource_fork(&dest).expect("read dest resource fork"),
        None,
        "ResourceFork must be stripped when -X is off"
    );
    assert_eq!(
        read_finder_info(&dest).expect("read dest finder info"),
        None,
        "FinderInfo must be stripped when -X is off"
    );
}

/// The sender never transmits rsync's own `rsync.%` channel under a single
/// `-X`, so a fresh destination starts without the source's copy of it. The
/// clone copied it, and `sync_xattrs` never deletes it.
///
/// upstream: `xattrs.c:272-280` `rsync_xal_get()`
#[test]
fn test_clonefile_strips_the_source_rsync_channel_when_xattrs_on() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_channel.txt");
    let dest = temp.path().join("dst_channel.txt");
    fs::write(&source, b"rsync channel data").expect("write source");
    xattr::set(&source, metadata::FAKE_SUPER_XATTR, b"100600 0,0 77:77").expect("write %stat");
    xattr::set(&source, "com.example.ordinary", b"kept").expect("write ordinary");

    clone_file(&source, &dest, LocalCopyOptions::new().xattrs(true));

    assert_eq!(
        xattr::get(&dest, metadata::FAKE_SUPER_XATTR).expect("read %stat"),
        None,
        "the source's rsync channel must not survive the clone"
    );
    assert_eq!(
        xattr::get(&dest, "com.example.ordinary").expect("read ordinary"),
        Some(b"kept".to_vec()),
        "an ordinary xattr must survive under -X"
    );
}

/// A selective xattr filter cannot be reproduced on a clone, so the fast path
/// must defer to the portable copy, which applies the per-name filter.
#[test]
fn test_clonefile_defers_to_the_copy_path_under_an_xattr_filter() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_filtered.txt");
    let dest = temp.path().join("dst_filtered.txt");
    fs::write(&source, b"filtered xattr data").expect("write source");
    xattr::set(&source, "com.example.keep", b"one").expect("write keep");
    xattr::set(&source, "com.example.drop", b"two").expect("write drop");

    let program = FilterProgram::new([FilterProgramEntry::Rule(
        FilterRule::exclude("com.example.drop").with_xattr_only(true),
    )])
    .expect("filter program");
    let summary = copy_file(
        &source,
        &dest,
        LocalCopyOptions::new()
            .xattrs(true)
            .with_filter_program(Some(program)),
    );

    assert!(
        summary
            .copy_method_breakdown()
            .iter()
            .all(|&(label, _)| label != CLONEFILE_LABEL),
        "an xattr filter must keep the copy off the clonefile fast path"
    );
    assert_eq!(
        xattr::get(&dest, "com.example.keep").expect("read keep"),
        Some(b"one".to_vec())
    );
    assert_eq!(
        xattr::get(&dest, "com.example.drop").expect("read drop"),
        None,
        "the filtered xattr must not reach the destination"
    );
}

/// Reports no reflink support so the copy takes the portable path, which is
/// the reference a clone must reproduce.
#[derive(Debug)]
struct PortableCopyOnly;

impl PlatformCopy for PortableCopyOnly {
    fn copy_file(&self, _src: &Path, _dst: &Path, _size_hint: u64) -> io::Result<CopyResult> {
        Ok(CopyResult::new(0, CopyMethod::StandardCopy))
    }

    fn supports_reflink(&self) -> bool {
        false
    }

    fn preferred_method(&self, _size: u64) -> CopyMethod {
        CopyMethod::StandardCopy
    }
}

fn fake_super_archive() -> LocalCopyOptions {
    LocalCopyOptions::new()
        .permissions(true)
        .times(true)
        .owner(true)
        .group(true)
        .fake_super(true)
}

/// Under `--fake-super` the receiver records the sender's view of the file in
/// `rsync.%stat`, and the clone must end exactly as the portable copy does.
///
/// The source carries its own `%stat` (mode 0600, owner 77:77), which the
/// sender reads as the file's identity. Upstream 3.5.1 writes
/// `rsync.%stat="100600 0,0 77:77"` on the destination (measured with
/// `rsync -a --fake-super`), because that identity differs from the
/// destination's real owner. The clone copied the source's `%stat` verbatim
/// and the post-clone strip removes it, so the value checked here must come
/// from finalize's fake-super write, not from the clone.
///
/// The destination's real mode is compared against the portable copy rather
/// than against upstream: upstream also chmods the real file to 0600, while
/// oc's portable path keeps the source mode. That is a separate, pre-existing
/// divergence; this test pins clone/portable parity.
///
/// upstream: `rsync.c:713-714` `set_file_attrs()` calls `set_stat_xattr()`
/// under `am_root < 0`; `xattrs.c:1273-1357` `set_stat_xattr()` formats
/// `"%o %u,%u %u:%u"` from the file-list entry.
#[test]
fn test_clonefile_matches_the_portable_copy_under_fake_super() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }
    let source = temp.path().join("src_fake_super.txt");
    let cloned = temp.path().join("dst_fake_super_clone.txt");
    let portable = temp.path().join("dst_fake_super_portable.txt");
    fs::write(&source, b"fake super data").expect("write source");
    xattr::set(&source, metadata::FAKE_SUPER_XATTR, b"100600 0,0 77:77").expect("write %stat");

    clone_file(&source, &cloned, fake_super_archive());
    let summary = copy_file(
        &source,
        &portable,
        fake_super_archive().with_platform_copy(Arc::new(PortableCopyOnly)),
    );
    assert!(
        summary
            .copy_method_breakdown()
            .iter()
            .all(|&(label, _)| label != CLONEFILE_LABEL),
        "the reference copy must take the portable path"
    );

    let cloned_stat = xattr::get(&cloned, metadata::FAKE_SUPER_XATTR).expect("read clone %stat");
    assert_eq!(
        cloned_stat,
        Some(b"100600 0,0 77:77".to_vec()),
        "the clone must carry the sender's identity, as upstream writes it"
    );
    assert_eq!(
        cloned_stat,
        xattr::get(&portable, metadata::FAKE_SUPER_XATTR).expect("read portable %stat"),
        "the clone and the portable copy must record the same identity"
    );
    let mode = |path: &Path| fs::metadata(path).expect("stat").permissions().mode() & 0o7777;
    assert_eq!(
        mode(&cloned),
        mode(&portable),
        "the clone and the portable copy must end with the same real mode"
    );
}
