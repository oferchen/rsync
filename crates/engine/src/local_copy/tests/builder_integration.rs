//! Integration tests demonstrating the builder pattern for LocalCopyOptions.
//!
//! These tests show how the builder pattern makes test setup cleaner compared
//! to the direct fluent API approach.

use std::path::PathBuf;

use crate::local_copy::{
    LocalCopyOptions, LocalCopyOptionsBuilder, LocalCopyPlan, LocalCopyExecution,
    ReferenceDirectory, ReferenceDirectoryKind,
};
use tempfile::tempdir;
use std::fs;

// ==================== Before: Using Fluent API Directly ====================
// These tests show the previous pattern (still valid, but more verbose)

#[test]
fn fluent_api_basic_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"hello").expect("write");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .times(true)
        .permissions(true);

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options).expect("copy");

    assert_eq!(fs::read(&dest).expect("read"), b"hello");
}

// ==================== After: Using Builder Pattern ====================
// These tests demonstrate cleaner setup with the builder

#[test]
fn builder_basic_copy() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let dest = temp.path().join("dest.txt");
    fs::write(&source, b"hello").expect("write");

    let options = LocalCopyOptions::builder()
        .recursive(true)
        .times(true)
        .permissions(true)
        .build()
        .expect("valid options");

    let operands = vec![source.into_os_string(), dest.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options).expect("copy");

    assert_eq!(fs::read(&dest).expect("read"), b"hello");
}

#[test]
fn builder_archive_preset() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"archive test").expect("write");

    // Archive preset enables recursive, symlinks, perms, times, owner, group, devices, specials
    let options = LocalCopyOptions::builder()
        .archive()
        .build()
        .expect("valid options");

    assert!(options.recursive_enabled());
    assert!(options.links_enabled());
    assert!(options.preserve_permissions());
    assert!(options.preserve_times());
    assert!(options.preserve_owner());
    assert!(options.preserve_group());
    assert!(options.devices_enabled());
    assert!(options.specials_enabled());

    let operands = vec![source_dir.into_os_string(), dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    plan.execute_with_options(LocalCopyExecution::Apply, options).expect("copy");

    assert_eq!(
        fs::read(dest_dir.join("file.txt")).expect("read"),
        b"archive test"
    );
}

#[test]
fn builder_sync_preset() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(source_dir.join("keep.txt"), b"keep me").expect("write keep");
    fs::write(dest_dir.join("delete.txt"), b"delete me").expect("write delete");

    // Sync preset enables archive + delete
    let options = LocalCopyOptions::builder()
        .sync()
        .build()
        .expect("valid options");

    assert!(options.recursive_enabled());
    assert!(options.delete_extraneous());

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_dir.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("sync");

    assert!(dest_dir.join("keep.txt").exists());
    assert!(!dest_dir.join("delete.txt").exists());
    assert!(summary.deletions() > 0);
}

#[test]
fn builder_backup_preset() {
    let options = LocalCopyOptions::builder()
        .backup_preset()
        .build()
        .expect("valid options");

    assert!(options.recursive_enabled());
    assert!(options.hard_links_enabled());
    assert!(options.partial_enabled());
}

#[test]
fn builder_with_validation_catches_conflicts() {
    // size_only and checksum are mutually exclusive
    let result = LocalCopyOptions::builder()
        .size_only(true)
        .checksum(true)
        .build();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("conflicting"));
}

#[test]
fn builder_min_max_file_size_validation() {
    // min > max is invalid
    let result = LocalCopyOptions::builder()
        .min_file_size(Some(1000))
        .max_file_size(Some(500))
        .build();

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.to_string().contains("min_file_size"));
}

#[test]
fn builder_unchecked_skips_validation() {
    // This would fail with build()
    let options = LocalCopyOptions::builder()
        .size_only(true)
        .checksum(true)
        .build_unchecked();

    // But build_unchecked bypasses validation
    assert!(options.size_only_enabled());
    assert!(options.checksum_enabled());
}

#[test]
fn builder_with_reference_directories_same_kind() {
    let options = LocalCopyOptions::builder()
        .reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            "/old/backup",
        ))
        .reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            "/previous/backup",
        ))
        .build()
        .expect("valid options");

    let refs = options.reference_directories();
    assert_eq!(refs.len(), 2);
    assert_eq!(refs[0].kind(), ReferenceDirectoryKind::Compare);
    assert_eq!(refs[1].kind(), ReferenceDirectoryKind::Compare);
}

#[test]
fn builder_rejects_mixed_reference_directory_kinds() {
    let result = LocalCopyOptions::builder()
        .reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Compare,
            "/old/backup",
        ))
        .reference_directory(ReferenceDirectory::new(
            ReferenceDirectoryKind::Link,
            "/previous/backup",
        ))
        .build();

    assert!(result.is_err(), "mixing --compare-dest and --link-dest should be rejected");
    let err = result.unwrap_err();
    assert!(err.to_string().contains("conflicting"));
}

#[test]
fn builder_chaining_modifies_preset() {
    let options = LocalCopyOptions::builder()
        .archive()
        .compress(true)
        .partial(true)
        .fsync(true)
        .build()
        .expect("valid options");

    // Archive settings
    assert!(options.recursive_enabled());
    assert!(options.links_enabled());

    // Additional customizations
    assert!(options.compress_enabled());
    assert!(options.partial_enabled());
    assert!(options.fsync_enabled());
}

#[test]
fn builder_link_dests() {
    let options = LocalCopyOptions::builder()
        .link_dest("/backup/v1")
        .link_dest("/backup/v2")
        .link_dests(["/backup/v3", "/backup/v4"])
        .build()
        .expect("valid options");

    assert_eq!(options.link_dest_entries().len(), 4);
}

#[test]
fn builder_default_values_match_local_copy_options_default() {
    let from_builder = LocalCopyOptions::builder().build().expect("valid options");
    let from_default = LocalCopyOptions::default();

    // Key defaults should match
    assert_eq!(
        from_builder.recursive_enabled(),
        from_default.recursive_enabled()
    );
    assert_eq!(
        from_builder.whole_file_enabled(),
        from_default.whole_file_enabled()
    );
    assert_eq!(
        from_builder.implied_dirs_enabled(),
        from_default.implied_dirs_enabled()
    );
    assert_eq!(
        from_builder.delete_extraneous(),
        from_default.delete_extraneous()
    );
    assert_eq!(
        from_builder.compress_enabled(),
        from_default.compress_enabled()
    );
}

#[test]
fn builder_cloneable() {
    let builder1 = LocalCopyOptions::builder()
        .archive()
        .delete(true);

    let builder2 = builder1.clone();

    let options1 = builder1.build().expect("valid options");
    let options2 = builder2.build().expect("valid options");

    assert_eq!(options1.delete_extraneous(), options2.delete_extraneous());
    assert_eq!(options1.recursive_enabled(), options2.recursive_enabled());
}

#[test]
fn builder_debug_format() {
    let builder = LocalCopyOptions::builder().archive();
    let debug = format!("{builder:?}");

    assert!(debug.contains("LocalCopyOptionsBuilder"));
}
