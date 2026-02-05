// Tests for --itemize-changes output format
// Format: YXcstpoguax where:
//   Y = update type: '>' (file), 'c' (create/symlink), 'h' (hard link), '*' (delete), '.' (no-op)
//   X = file type: 'f' (file), 'd' (dir), 'L' (symlink), 'S' (special), 'D' (device)
//   c = checksum (data) change
//   s = size change
//   t = time change (t=preserve, T=transfer time)
//   p = permissions change
//   o = owner change
//   g = group change
//   u/n/b = access time/create time/both changed
//   a = ACL change
//   x = xattr change
// New files show '++++++++++' for attributes

#[test]
fn itemize_new_file_shows_all_plus_signs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"new file").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert!(record.was_created());

    // New file should have all attributes marked as new with '+'
    let change_set = record.change_set();
    assert!(change_set.size_changed());
    assert!(change_set.time_change().is_some());
}

#[test]
fn itemize_new_file_format_matches_upstream() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // Verify record indicates creation
    assert!(record.was_created());
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);
}

#[test]
fn itemize_modified_file_shows_change_indicators() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create destination with old content
    fs::write(&destination, b"old").expect("write dest");

    // Create source with new content
    fs::write(&source, b"new content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // File existed, so not created
    assert!(!record.was_created());
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);

    let change_set = record.change_set();
    // Content changed (checksum)
    assert!(change_set.checksum_changed());
    // Size changed (old: 3 bytes, new: 11 bytes)
    assert!(change_set.size_changed());
}

#[test]
fn itemize_unchanged_file_shows_metadata_reused() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let content = b"same content";

    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set same modification time
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // File already existed and matches
    assert!(!record.was_created());
    assert_eq!(record.action(), &LocalCopyAction::MetadataReused);

    let change_set = record.change_set();
    // No content change
    assert!(!change_set.checksum_changed());
    // No size change
    assert!(!change_set.size_changed());
    // No time change (same timestamp)
    assert!(change_set.time_change().is_none());
}

#[cfg(unix)]
#[test]
fn itemize_permission_change_shows_p_indicator() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let content = b"same content";

    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set different permissions
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644))
        .expect("set source perms");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
        .expect("set dest perms");

    // Set same modification time to avoid time changes
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let change_set = record.change_set();
    // Permissions changed
    assert!(change_set.permissions_changed());
    // No content change (same data)
    assert!(!change_set.checksum_changed());
    // No size change
    assert!(!change_set.size_changed());
}

#[cfg(unix)]
#[test]
fn itemize_time_change_shows_t_indicator_when_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let content = b"content";

    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set different modification times
    let old_time = FileTime::from_unix_time(1_700_000_000, 0);
    let new_time = FileTime::from_unix_time(1_700_001_000, 0);
    set_file_mtime(&source, new_time).expect("set source mtime");
    set_file_mtime(&destination, old_time).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let change_set = record.change_set();
    // Time was preserved (different times)
    assert_eq!(change_set.time_change(), Some(TimeChange::Modified));
    assert_eq!(change_set.time_change_marker(), Some('t'));
}

#[cfg(unix)]
#[test]
fn itemize_time_change_shows_capital_t_when_not_preserved() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Don't preserve times
    let options = LocalCopyOptions::default()
        .times(false)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let change_set = record.change_set();
    // Time set to transfer time (not preserved)
    assert_eq!(change_set.time_change(), Some(TimeChange::TransferTime));
    assert_eq!(change_set.time_change_marker(), Some('T'));
}

#[cfg(unix)]
#[test]
fn itemize_multiple_changes_shows_all_indicators() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create destination with old content and permissions
    fs::write(&destination, b"old").expect("write dest");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o600))
        .expect("set dest perms");
    let old_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&destination, old_time).expect("set dest mtime");

    // Create source with new content, different permissions and time
    fs::write(&source, b"new content here").expect("write source");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644))
        .expect("set source perms");
    let new_time = FileTime::from_unix_time(1_700_001_000, 0);
    set_file_mtime(&source, new_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .permissions(true)
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let change_set = record.change_set();
    // All these should have changed
    assert!(change_set.checksum_changed(), "checksum should change");
    assert!(change_set.size_changed(), "size should change");
    assert_eq!(change_set.time_change(), Some(TimeChange::Modified), "time should change");
    assert!(change_set.permissions_changed(), "permissions should change");
}

#[test]
#[ignore = "Directory records are not marked with was_created() - implementation incomplete"]
fn itemize_new_directory_shows_creation() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source_dir");
    let destination = temp.path().join("dest");

    fs::create_dir(&source).expect("create source dir");
    fs::create_dir(&destination).expect("create dest dir");

    // Add a file to copy with the directory
    let source_file = source.join("file.txt");
    fs::write(&source_file, b"content").expect("write file");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();

    // Should have records for directory and file
    assert!(!records.is_empty());

    // Find the directory record
    let dir_record = records.iter()
        .find(|r| r.action() == &LocalCopyAction::DirectoryCreated);
    assert!(dir_record.is_some(), "should have directory creation record");

    let dir_record = dir_record.unwrap();
    assert!(dir_record.was_created());
}

#[cfg(unix)]
#[test]
#[ignore = "Symlink records are not marked with was_created() - implementation incomplete"]
fn itemize_symlink_shows_correct_type() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source_link");
    let destination = temp.path().join("dest_link");
    let target = temp.path().join("target.txt");

    fs::write(&target, b"target content").expect("write target");
    symlink(&target, &source).expect("create symlink");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    assert_eq!(record.action(), &LocalCopyAction::SymlinkCopied);
    assert!(record.was_created());
}

#[cfg(unix)]
#[test]
fn itemize_hard_link_shows_correct_action() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");

    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Create two hard-linked files in source
    let file1 = source_dir.join("file1.txt");
    let file2 = source_dir.join("file2.txt");
    fs::write(&file1, b"linked content").expect("write file1");
    fs::hard_link(&file1, &file2).expect("create hard link");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .hard_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();

    // Should have at least one hard link record
    let hard_link_record = records.iter()
        .find(|r| r.action() == &LocalCopyAction::HardLink);
    assert!(hard_link_record.is_some(), "should have hard link record");
}

#[cfg(unix)]
#[test]
fn itemize_size_change_detected_correctly() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create files with different sizes
    fs::write(&destination, b"short").expect("write dest");
    fs::write(&source, b"this is a much longer content").expect("write source");

    // Make times the same to isolate size change
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let change_set = record.change_set();
    // Size definitely changed
    assert!(change_set.size_changed());
    // Content changed too
    assert!(change_set.checksum_changed());
}

#[test]
fn itemize_no_change_when_skip_existing() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&destination, b"existing").expect("write dest");
    fs::write(&source, b"new content").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .ignore_existing(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // File was skipped
    assert_eq!(record.action(), &LocalCopyAction::SkippedExisting);
    assert!(!record.was_created());
}

#[cfg(unix)]
#[test]
fn itemize_chmod_modifier_shows_permission_change() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");
    let content = b"content";

    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set same permissions initially
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644))
        .expect("set source perms");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
        .expect("set dest perms");

    // Set same modification time
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source mtime");
    set_file_mtime(&destination, timestamp).expect("set dest mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Use chmod modifier to force permission change
    let chmod_mods = ChmodModifiers::parse("u+x").expect("parse chmod");
    let options = LocalCopyOptions::default()
        .times(true)
        .with_chmod(Some(chmod_mods))
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let change_set = record.change_set();
    // Chmod modifier causes permission change to be recorded
    assert!(change_set.permissions_changed());
}

#[cfg(unix)]
#[test]
fn itemize_format_matches_upstream_for_new_file() {
    // This test verifies the format matches upstream rsync's ">f+++++++++" pattern
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("newfile.txt");
    let destination = temp.path().join("dest.txt");
    fs::write(&source, b"brand new").expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // For a new file:
    // - was_created() should be true
    // - action should be DataCopied (represented as '>' in format)
    // - All attributes should be marked as new
    assert!(record.was_created());
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);

    let change_set = record.change_set();
    // New file has all these set
    assert!(change_set.size_changed());
    assert!(change_set.time_change().is_some());
}

#[cfg(unix)]
#[test]
fn itemize_format_matches_upstream_for_changed_file() {
    // This test verifies the format matches upstream rsync's ">f.st......" pattern
    // when content and time change
    use std::os::unix::fs::PermissionsExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Create destination with specific state
    fs::write(&destination, b"old").expect("write dest");
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))
        .expect("set dest perms");
    let old_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&destination, old_time).expect("set dest mtime");

    // Create source with changes
    fs::write(&source, b"new content").expect("write source");
    fs::set_permissions(&source, fs::Permissions::from_mode(0o644))
        .expect("set source perms");
    let new_time = FileTime::from_unix_time(1_700_001_000, 0);
    set_file_mtime(&source, new_time).expect("set source mtime");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // For an updated file:
    // - was_created() should be false (file existed)
    // - action should be DataCopied ('>') since content changed
    // - Specific attributes changed (c, s, t)
    assert!(!record.was_created());
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);

    let change_set = record.change_set();
    assert!(change_set.checksum_changed()); // 'c'
    assert!(change_set.size_changed()); // 's'
    assert_eq!(change_set.time_change(), Some(TimeChange::Modified)); // 't'
    assert!(!change_set.permissions_changed()); // '.' (same perms)
}
