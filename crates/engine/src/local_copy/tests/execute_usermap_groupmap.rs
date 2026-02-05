// Tests for --usermap and --groupmap functionality.
//
// The --usermap and --groupmap flags allow remapping of user and group ownership
// during file transfer operations. These mappings support various formats:
// - Numeric ID mappings: 1000:2000
// - Name mappings: alice:bob
// - Wildcard mappings: *:nobody
// - Pattern mappings: test*:backup
// - Range mappings: 1000-2000:3000
// - Multiple mappings: 1000:2000,*:nobody
//
// Test cases covered:
// 1. User mapping with numeric IDs (source_uid:dest_uid)
// 2. Group mapping with numeric IDs (source_gid:dest_gid)
// 3. Multiple mappings specified together
// 4. Wildcard mappings (*:nobody)
// 5. Pattern-based mappings with wildcards
// 6. Range-based mappings
// 7. Combining mappings with other metadata preservation flags
// 8. Error cases and edge conditions

// ============================================================================
// Basic User Mapping Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn usermap_remaps_numeric_uid() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root (cannot set arbitrary UIDs)
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 1000
    let source_uid = 1000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    // Map UID 1000 -> 2000
    let user_mapping = ::metadata::UserMapping::parse("1000:2000").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped UID 2000
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 2000);
}

#[cfg(unix)]
#[test]
fn usermap_wildcard_maps_all_users() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 5000
    let source_uid = 5000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    // Map all users to UID 9999 using wildcard
    let user_mapping = ::metadata::UserMapping::parse("*:9999").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped UID 9999
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 9999);
}

#[cfg(unix)]
#[test]
fn usermap_multiple_rules_first_match_wins() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 1000
    let source_uid = 1000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    // Map with multiple rules: 1000:2000 takes precedence over wildcard
    let user_mapping = ::metadata::UserMapping::parse("1000:2000,*:9999").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped UID 2000 (first matching rule)
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 2000);
}

#[cfg(unix)]
#[test]
fn usermap_range_mapping() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 1500 (within range 1000-2000)
    let source_uid = 1500;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    // Map range 1000-2000 -> 3000
    let user_mapping = ::metadata::UserMapping::parse("1000-2000:3000").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped UID 3000
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 3000);
}

#[cfg(unix)]
#[test]
fn usermap_no_match_preserves_original_uid() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 5000
    let source_uid = 5000;
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(source_uid)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    // Map only UID 1000 -> 2000 (5000 won't match)
    let user_mapping = ::metadata::UserMapping::parse("1000:2000").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination preserves original UID 5000
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 5000);
}

// ============================================================================
// Basic Group Mapping Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn groupmap_remaps_numeric_gid() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to GID 1000
    let source_gid = 1000;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    // Map GID 1000 -> 2000
    let group_mapping = ::metadata::GroupMapping::parse("1000:2000").expect("parse groupmap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped GID 2000
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.gid(), 2000);
}

#[cfg(unix)]
#[test]
fn groupmap_wildcard_maps_all_groups() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to GID 5000
    let source_gid = 5000;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    // Map all groups to GID 9999 using wildcard
    let group_mapping = ::metadata::GroupMapping::parse("*:9999").expect("parse groupmap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped GID 9999
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.gid(), 9999);
}

#[cfg(unix)]
#[test]
fn groupmap_multiple_rules() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source1 = temp.path().join("file1.txt");
    let source2 = temp.path().join("file2.txt");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    fs::write(&source1, b"content1").expect("write source1");
    fs::write(&source2, b"content2").expect("write source2");

    // Set different GIDs for the source files
    chownat(
        rustix::fs::CWD,
        &source1,
        None,
        Some(unix_ids::gid(1000)),
        AtFlags::empty(),
    )
    .expect("set source1 gid");

    chownat(
        rustix::fs::CWD,
        &source2,
        None,
        Some(unix_ids::gid(3000)),
        AtFlags::empty(),
    )
    .expect("set source2 gid");

    // Map with multiple rules: 1000:2000, 3000:4000, *:9999
    let group_mapping =
        ::metadata::GroupMapping::parse("1000:2000,3000:4000,*:9999").expect("parse groupmap");

    // Copy both files
    let operands1 = vec![
        source1.into_os_string(),
        dest_dir.join("file1.txt").into_os_string(),
    ];
    let plan1 = LocalCopyPlan::from_operands(&operands1).expect("plan1");
    plan1
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .with_group_mapping(Some(group_mapping.clone())),
        )
        .expect("copy1 succeeds");

    let operands2 = vec![
        source2.into_os_string(),
        dest_dir.join("file2.txt").into_os_string(),
    ];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan2");
    plan2
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy2 succeeds");

    // Verify destinations have correct mapped GIDs
    let dest1_metadata = fs::metadata(dest_dir.join("file1.txt")).expect("dest1 metadata");
    assert_eq!(dest1_metadata.gid(), 2000);

    let dest2_metadata = fs::metadata(dest_dir.join("file2.txt")).expect("dest2 metadata");
    assert_eq!(dest2_metadata.gid(), 4000);
}

#[cfg(unix)]
#[test]
fn groupmap_range_mapping() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to GID 1500 (within range 1000-2000)
    let source_gid = 1500;
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(source_gid)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    // Map range 1000-2000 -> 3000
    let group_mapping = ::metadata::GroupMapping::parse("1000-2000:3000").expect("parse groupmap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify destination has mapped GID 3000
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.gid(), 3000);
}

// ============================================================================
// Combined User and Group Mapping Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn usermap_and_groupmap_work_together() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 1000, GID 1000
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(1000)),
        Some(unix_ids::gid(1000)),
        AtFlags::empty(),
    )
    .expect("set source ownership");

    // Map UID 1000 -> 2000 and GID 1000 -> 3000
    let user_mapping = ::metadata::UserMapping::parse("1000:2000").expect("parse usermap");
    let group_mapping = ::metadata::GroupMapping::parse("1000:3000").expect("parse groupmap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .with_user_mapping(Some(user_mapping))
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify both UID and GID are mapped correctly
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 2000);
    assert_eq!(dest_metadata.gid(), 3000);
}

#[cfg(unix)]
#[test]
fn usermap_and_groupmap_with_wildcards() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to arbitrary UID/GID
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(5000)),
        Some(unix_ids::gid(6000)),
        AtFlags::empty(),
    )
    .expect("set source ownership");

    // Map all users to 9998 and all groups to 9999
    let user_mapping = ::metadata::UserMapping::parse("*:9998").expect("parse usermap");
    let group_mapping = ::metadata::GroupMapping::parse("*:9999").expect("parse groupmap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .group(true)
                .with_user_mapping(Some(user_mapping))
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify both UID and GID are mapped to wildcard values
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 9998);
    assert_eq!(dest_metadata.gid(), 9999);
}

// ============================================================================
// Mapping Without Preservation Flags
// ============================================================================

#[cfg(unix)]
#[test]
fn usermap_requires_owner_preservation_flag() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to UID 1000
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(1000)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    // Map UID 1000 -> 2000 but DON'T enable owner preservation
    let user_mapping = ::metadata::UserMapping::parse("1000:2000").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                // Note: owner(true) is NOT set
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Without owner preservation, mapping should not be applied
    // The file will have the current effective user's UID
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let current_uid = rustix::process::geteuid().as_raw();
    assert_eq!(dest_metadata.uid(), current_uid);
}

#[cfg(unix)]
#[test]
fn groupmap_requires_group_preservation_flag() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source file to GID 1000
    chownat(
        rustix::fs::CWD,
        &source,
        None,
        Some(unix_ids::gid(1000)),
        AtFlags::empty(),
    )
    .expect("set source gid");

    // Map GID 1000 -> 2000 but DON'T enable group preservation
    let group_mapping = ::metadata::GroupMapping::parse("1000:2000").expect("parse groupmap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                // Note: group(true) is NOT set
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Without group preservation, mapping should not be applied
    // The file will have the current effective group's GID
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    let current_gid = rustix::process::getegid().as_raw();
    assert_eq!(dest_metadata.gid(), current_gid);
}

// ============================================================================
// Recursive Directory Tests
// ============================================================================

#[cfg(unix)]
#[test]
fn usermap_applies_to_directory_tree() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");

    // Create a directory structure
    let file1 = source_dir.join("file1.txt");
    let subdir = source_dir.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");
    let file2 = subdir.join("file2.txt");
    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");

    // Set all files to UID 1000
    for path in [&source_dir, &file1, &subdir, &file2] {
        chownat(
            rustix::fs::CWD,
            path,
            Some(unix_ids::uid(1000)),
            None,
            AtFlags::empty(),
        )
        .expect("set ownership");
    }

    // Map UID 1000 -> 2000
    let user_mapping = ::metadata::UserMapping::parse("1000:2000").expect("parse usermap");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .recursive(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert!(summary.files_copied() >= 2); // At least the two files

    // Verify all files have mapped UID 2000
    let dest_file1 = dest_dir.join("file1.txt");
    let dest_file2 = dest_dir.join("subdir").join("file2.txt");
    let dest_subdir = dest_dir.join("subdir");

    assert_eq!(fs::metadata(&dest_file1).expect("file1 metadata").uid(), 2000);
    assert_eq!(fs::metadata(&dest_file2).expect("file2 metadata").uid(), 2000);
    assert_eq!(fs::metadata(&dest_subdir).expect("subdir metadata").uid(), 2000);
}

#[cfg(unix)]
#[test]
fn groupmap_applies_to_directory_tree() {
    use std::os::unix::fs::MetadataExt;
    use rustix::fs::{AtFlags, chownat};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&source_dir).expect("create source");

    // Create a directory structure
    let file1 = source_dir.join("file1.txt");
    let subdir = source_dir.join("subdir");
    fs::create_dir_all(&subdir).expect("create subdir");
    let file2 = subdir.join("file2.txt");
    fs::write(&file1, b"content1").expect("write file1");
    fs::write(&file2, b"content2").expect("write file2");

    // Set all files to GID 1000
    for path in [&source_dir, &file1, &subdir, &file2] {
        chownat(
            rustix::fs::CWD,
            path,
            None,
            Some(unix_ids::gid(1000)),
            AtFlags::empty(),
        )
        .expect("set ownership");
    }

    // Map GID 1000 -> 3000
    let group_mapping = ::metadata::GroupMapping::parse("1000:3000").expect("parse groupmap");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .group(true)
                .recursive(true)
                .with_group_mapping(Some(group_mapping)),
        )
        .expect("copy succeeds");

    assert!(summary.files_copied() >= 2); // At least the two files

    // Verify all files have mapped GID 3000
    let dest_file1 = dest_dir.join("file1.txt");
    let dest_file2 = dest_dir.join("subdir").join("file2.txt");
    let dest_subdir = dest_dir.join("subdir");

    assert_eq!(fs::metadata(&dest_file1).expect("file1 metadata").gid(), 3000);
    assert_eq!(fs::metadata(&dest_file2).expect("file2 metadata").gid(), 3000);
    assert_eq!(fs::metadata(&dest_subdir).expect("subdir metadata").gid(), 3000);
}

// ============================================================================
// Interaction with Other Options
// ============================================================================

#[cfg(unix)]
#[test]
fn usermap_works_with_permissions_and_times() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use rustix::fs::{AtFlags, chownat};
    use filetime::{FileTime, set_file_times};

    // Skip test if not running as root
    if rustix::process::geteuid().as_raw() != 0 {
        return;
    }

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    // Set source metadata
    chownat(
        rustix::fs::CWD,
        &source,
        Some(unix_ids::uid(1000)),
        None,
        AtFlags::empty(),
    )
    .expect("set source uid");

    fs::set_permissions(&source, PermissionsExt::from_mode(0o640)).expect("set perms");
    let mtime = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&source, mtime, mtime).expect("set times");

    // Map UID and preserve permissions and times
    let user_mapping = ::metadata::UserMapping::parse("1000:2000").expect("parse usermap");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .owner(true)
                .permissions(true)
                .times(true)
                .with_user_mapping(Some(user_mapping)),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify all metadata is preserved correctly
    let dest_metadata = fs::metadata(&destination).expect("dest metadata");
    assert_eq!(dest_metadata.uid(), 2000);
    assert_eq!(dest_metadata.permissions().mode() & 0o777, 0o640);
    let dest_mtime = FileTime::from_last_modification_time(&dest_metadata);
    assert_eq!(dest_mtime, mtime);
}
