#![cfg(target_os = "macos")]

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use apple_fs::{
    FINDER_INFO_LEN, read_finder_info, read_resource_fork, write_finder_info, write_resource_fork,
};
use engine::local_copy::clonefile::{CloneResult, clone_or_copy};
use engine::local_copy::{LocalCopyExecution, LocalCopyOptions, LocalCopyPlan};
use tempfile::tempdir;

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

    // Set historical creation time (year 2020: 1_600_000_000 secs).
    let historical_secs: i64 = 1_600_000_000;
    metadata::set_crtime(&source, historical_secs).expect("set source crtime");

    let src_meta = fs::metadata(&source).expect("stat source");
    let src_crtime = src_meta
        .created()
        .expect("source crtime")
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("duration")
        .as_secs() as i64;
    assert_eq!(
        src_crtime, historical_secs,
        "source creation time must match historical value"
    );

    let before_copy = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("duration")
        .as_secs() as i64;

    // Transfer with --crtimes OFF (default)
    let operands_off = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest_off.as_os_str()),
    ];
    let plan_off = LocalCopyPlan::from_operands(&operands_off).expect("plan off");
    let options_off = LocalCopyOptions::new().crtimes(false).times(true);
    plan_off
        .execute_with_options(LocalCopyExecution::Apply, options_off)
        .expect("execute copy with crtimes off");

    assert!(dest_off.exists(), "destination file must exist");
    let dest_off_meta = fs::metadata(&dest_off).expect("stat dest off");
    let dest_off_crtime = dest_off_meta
        .created()
        .expect("dest crtime")
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("duration")
        .as_secs() as i64;

    // Destination creation time should be normalized to current time, NOT historical value.
    assert!(
        dest_off_crtime >= before_copy - 5,
        "creation time when --crtimes is off should be current time ({dest_off_crtime} >= {before_copy} - 5)"
    );
    assert_ne!(
        dest_off_crtime, historical_secs,
        "cloned destination must not retain historical creation time when --crtimes is off"
    );

    // Transfer with --crtimes ON
    let operands_on = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest_on.as_os_str()),
    ];
    let plan_on = LocalCopyPlan::from_operands(&operands_on).expect("plan on");
    let options_on = LocalCopyOptions::new().crtimes(true).times(true);
    plan_on
        .execute_with_options(LocalCopyExecution::Apply, options_on)
        .expect("execute copy with crtimes on");

    assert!(dest_on.exists(), "destination file must exist");
    let dest_on_meta = fs::metadata(&dest_on).expect("stat dest on");
    let dest_on_crtime = dest_on_meta
        .created()
        .expect("dest crtime")
        .duration_since(SystemTime::UNIX_EPOCH)
        .expect("duration")
        .as_secs() as i64;

    assert_eq!(
        dest_on_crtime, historical_secs,
        "destination creation time must match source when --crtimes is on"
    );
}

#[test]
fn test_clonefile_acls_stripped_when_acls_off() {
    let temp = tempdir().expect("tempdir");
    if !apfs_clonefile_supported(temp.path()) {
        eprintln!("APFS clonefile not supported on this volume, skipping test");
        return;
    }

    let source = temp.path().join("src_acl.txt");
    let dest_no_acl = temp.path().join("dst_no_acl.txt");
    let dest_with_acl = temp.path().join("dst_with_acl.txt");

    fs::write(&source, b"acl clone content").expect("write source");

    // Add an extended ACL entry to source.
    let acl_entries = vec![exacl::AclEntry::allow_user(
        "nobody",
        exacl::Perm::READ,
        exacl::Flag::empty(),
    )];
    if let Err(err) = exacl::setfacl(&[&source], &acl_entries, None) {
        eprintln!("setfacl not supported or permission denied: {err}, skipping ACL test");
        return;
    }

    let src_acl = exacl::getfacl(&source, None).expect("getfacl source");
    assert!(!src_acl.is_empty(), "source must have extended ACL entries");

    // Transfer with -A OFF
    let operands_no_acl = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest_no_acl.as_os_str()),
    ];
    let plan_no_acl = LocalCopyPlan::from_operands(&operands_no_acl).expect("plan no acl");
    let options_no_acl = LocalCopyOptions::new().acls(false);
    plan_no_acl
        .execute_with_options(LocalCopyExecution::Apply, options_no_acl)
        .expect("execute copy without acls");

    assert!(dest_no_acl.exists(), "destination without acls must exist");
    let dst_acl = exacl::getfacl(&dest_no_acl, None).expect("getfacl dest no acl");
    assert!(
        dst_acl.is_empty(),
        "cloned destination must strip extended ACL entries when -A is off: {dst_acl:?}"
    );

    // Transfer with -A ON
    let operands_with_acl = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest_with_acl.as_os_str()),
    ];
    let plan_with_acl = LocalCopyPlan::from_operands(&operands_with_acl).expect("plan with acl");
    let options_with_acl = LocalCopyOptions::new().acls(true);
    plan_with_acl
        .execute_with_options(LocalCopyExecution::Apply, options_with_acl)
        .expect("execute copy with acls");

    assert!(dest_with_acl.exists(), "destination with acls must exist");
    let dst_preserved_acl = exacl::getfacl(&dest_with_acl, None).expect("getfacl dest with acl");
    assert!(
        !dst_preserved_acl.is_empty(),
        "cloned destination must preserve extended ACL entries when -A is on"
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

    // Copy with -X ON
    let operands = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest.as_os_str()),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::new().xattrs(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute copy with xattrs on");

    assert!(dest.exists(), "destination must exist");
    let read_rf = read_resource_fork(&dest).expect("read dest resource fork");
    assert_eq!(
        read_rf,
        Some(rf_payload),
        "ResourceFork must be preserved exactly when -X is on"
    );

    let read_fi = read_finder_info(&dest).expect("read dest finder info");
    assert_eq!(
        read_fi,
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

    // Multi-kilobyte resource fork payload
    let large_rf = vec![0xa5u8; 2048];
    write_resource_fork(&source, &large_rf).expect("write large resource fork");

    let operands = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest.as_os_str()),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::new().xattrs(true);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute copy with large resource fork");

    assert!(dest.exists(), "destination must exist");
    let read_rf = read_resource_fork(&dest).expect("read dest large resource fork");
    assert_eq!(
        read_rf.as_ref().map(Vec::len),
        Some(large_rf.len()),
        "large ResourceFork length must match"
    );
    assert_eq!(read_rf, Some(large_rf));
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

    let rf_payload = b"macOS ResourceFork to be stripped".to_vec();
    write_resource_fork(&source, &rf_payload).expect("write resource fork");

    let finder_info = [0x33u8; FINDER_INFO_LEN];
    write_finder_info(&source, &finder_info).expect("write finder info");

    // Copy with -X OFF
    let operands = vec![
        OsString::from(source.as_os_str()),
        OsString::from(dest.as_os_str()),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::new().xattrs(false);
    plan.execute_with_options(LocalCopyExecution::Apply, options)
        .expect("execute copy with xattrs off");

    assert!(dest.exists(), "destination must exist");
    let read_rf = read_resource_fork(&dest).expect("read dest resource fork");
    assert_eq!(
        read_rf, None,
        "ResourceFork must be stripped when -X is off"
    );

    let read_fi = read_finder_info(&dest).expect("read dest finder info");
    assert_eq!(read_fi, None, "FinderInfo must be stripped when -X is off");
}
