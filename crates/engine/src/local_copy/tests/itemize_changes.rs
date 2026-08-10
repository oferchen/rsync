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

    // upstream: generator.c:1942 - the position-2 `c` glyph fires only under
    // `--checksum`; enable it here so `checksum_changed()` reflects the
    // rewritten data.
    let options = LocalCopyOptions::default()
        .checksum(true)
        .collect_events(true);
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

    // upstream: generator.c:1942 - position-2 `c` is reserved for `--checksum`
    // mode, so enable it here to assert the multi-indicator pattern.
    let options = LocalCopyOptions::default()
        .checksum(true)
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

// upstream: hlink.c:228-234 maybe_hard_link() + log.c:736-738 - a hard-link
// follower whose destination does not yet exist is itemized through
// atomic_create() with the follower's own negative statret, which sets
// ITEM_IS_NEW and renders `hf+++++++++`. Regression guard for the follower
// that previously collapsed to blank attribute slots when created into an
// empty destination.
#[cfg(unix)]
#[test]
fn itemize_new_hard_link_follower_marked_created() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");

    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file1 = source_dir.join("a");
    let file2 = source_dir.join("b");
    fs::write(&file1, b"linked content").expect("write a");
    fs::hard_link(&file1, &file2).expect("hard link b -> a");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
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

    // The data-holder leader is a genuinely new file (`>f+++++++++`).
    let leader = records
        .iter()
        .find(|r| r.action() == &LocalCopyAction::DataCopied)
        .expect("leader data-copy record");
    assert!(leader.was_created(), "leader must itemize as created");

    // The follower is hard-linked into a previously empty destination, so
    // its own destination did not exist: upstream marks ITEM_IS_NEW and it
    // must render `hf+++++++++`, not a blank attribute run.
    let follower = records
        .iter()
        .find(|r| r.action() == &LocalCopyAction::HardLink)
        .expect("follower hard-link record");
    assert!(
        follower.was_created(),
        "new hard-link follower must be marked created so it itemizes hf+++++++++"
    );
}

// upstream: hlink.c:match_gnums / generator.c:1803-1806 / hlink.c:474-521
// finish_hard_link() - the LAST name-sorted member of a hard-link cohort is the
// transferred data-holder; every earlier member is deferred and then linked to
// the holder, so the itemize stream is `>f <last>` followed by the remaining
// members in descending name order, each pointing at the holder
// (`hf ... => <last>`). Regression guard: oc previously made the first-processed
// (first name-sorted) member the data-holder and chained aliases to the most
// recent one (`c => b`, `b => a`) instead of the star `b => c`, `a => c`.
#[cfg(unix)]
#[test]
fn itemize_hard_link_data_holder_is_last_name_sorted_member() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let dest_dir = temp.path().join("dest");

    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Three-member cohort {a, b, c} sharing one inode, created into an empty
    // destination.
    let a = source_dir.join("a");
    let b = source_dir.join("b");
    let c = source_dir.join("c");
    fs::write(&a, b"shared content").expect("write a");
    fs::hard_link(&a, &b).expect("hard link b -> a");
    fs::hard_link(&a, &c).expect("hard link c -> a");

    // A trailing slash on the source keeps the records at bare `a`/`b`/`c`
    // instead of a `source/`-prefixed relative path.
    let mut source_operand = source_dir.into_os_string();
    source_operand.push("/");
    let operands = vec![source_operand, dest_dir.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .hard_links(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();

    let base = |r: &LocalCopyRecord| -> String {
        r.relative_path()
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf8 basename")
            .to_string()
    };

    // The data-holder is the last name-sorted member `c`, itemized as a new
    // data copy (`>f+++++++++`).
    let holder = records
        .iter()
        .find(|r| r.action() == &LocalCopyAction::DataCopied)
        .expect("data-holder record");
    assert_eq!(
        base(holder),
        "c",
        "data-holder must be the last name-sorted member"
    );
    assert!(holder.was_created(), "holder must itemize as created");

    // The remaining members `b` and `a` are followers, both pointing at the
    // holder `c` (a star, not a chain), and each itemized `hf+++++++++`.
    let followers: Vec<&_> = records
        .iter()
        .filter(|r| r.action() == &LocalCopyAction::HardLink)
        .collect();
    assert_eq!(followers.len(), 2, "two hard-link followers expected");
    for follower in &followers {
        assert!(
            matches!(base(follower).as_str(), "a" | "b"),
            "unexpected follower name: {:?}",
            follower.relative_path()
        );
        assert!(
            follower.was_created(),
            "new hard-link follower must itemize hf+++++++++"
        );
        let leader = follower
            .metadata()
            .and_then(|m| m.symlink_target())
            .expect("follower carries a => holder trailer");
        assert_eq!(
            leader.file_name().and_then(|n| n.to_str()),
            Some("c"),
            "every follower must point at the data-holder `c`, not a chained alias"
        );
    }

    // Emission order mirrors upstream: holder first, then followers in
    // descending name order (`c`, `b`, `a`).
    let order: Vec<String> = records
        .iter()
        .filter(|r| {
            matches!(
                r.action(),
                LocalCopyAction::DataCopied | LocalCopyAction::HardLink
            )
        })
        .map(base)
        .collect();
    assert_eq!(order, ["c", "b", "a"], "holder-then-descending order");
}

// upstream: hlink.c:385-414 hard_link_check() + generator.c:995-1052
// try_dests_reg() - when a hard-link follower's destination is absent but its
// name matches a --copy-dest basis (quick_check_ok), statret is bumped from < 0
// to 1, so the itemize (log.c:736-738) never sets ITEM_IS_NEW: the follower
// renders blank (`hf          `), mirroring the leader that was itself satisfied
// from the copy-dest tree. Regression guard so the empty-destination
// new-follower fix does not wrongly mark a copy-dest-matched follower created.
#[cfg(unix)]
#[test]
fn itemize_copy_dest_hard_link_follower_not_marked_created() {
    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("source");
    let copy_dest_dir = temp.path().join("copy_dest");
    let dest_dir = temp.path().join("dest");

    fs::create_dir(&source_dir).expect("create source dir");
    fs::create_dir(&copy_dest_dir).expect("create copy_dest dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Source group leader and follower share one inode.
    let leader = source_dir.join("config1");
    let follower = source_dir.join("extra");
    fs::write(&leader, b"shared content").expect("write leader");
    fs::hard_link(&leader, &follower).expect("hard link extra -> config1");

    // The destination mirrors the source directory name (`dest/source/...`),
    // so the copy-dest basis is resolved at the same relative path. Hold
    // matching content for both names so the quick-check passes for the
    // follower's own basis.
    let copy_dest_source = copy_dest_dir.join("source");
    fs::create_dir(&copy_dest_source).expect("create copy_dest/source");
    let copy_leader = copy_dest_source.join("config1");
    let copy_follower = copy_dest_source.join("extra");
    fs::write(&copy_leader, b"shared content").expect("write copy_dest leader");
    fs::write(&copy_follower, b"shared content").expect("write copy_dest follower");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&leader, timestamp).expect("source leader mtime");
    set_file_mtime(&follower, timestamp).expect("source follower mtime");
    set_file_mtime(&copy_leader, timestamp).expect("copy_dest leader mtime");
    set_file_mtime(&copy_follower, timestamp).expect("copy_dest follower mtime");

    let operands = vec![
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .times(true)
        .hard_links(true)
        .collect_events(true)
        .extend_reference_directories([ReferenceDirectory::new(
            ReferenceDirectoryKind::Copy,
            &copy_dest_dir,
        )]);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let records = report.records();

    // The follower is hard-linked to a leader that was itself matched from
    // --copy-dest, so upstream leaves ITEM_IS_NEW unset: it must render blank
    // (`hf          `), never `hf+++++++++`.
    let follower_record = records
        .iter()
        .find(|r| r.action() == &LocalCopyAction::HardLink)
        .expect("follower hard-link record");
    assert!(
        !follower_record.was_created(),
        "copy-dest hard-link follower must stay blank, not marked created"
    );
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

    // The position-2 `c` glyph (ITEM_REPORT_CHANGE) only fires under
    // --checksum (upstream: generator.c:1929 - `if (always_checksum > 0)
    // iflags |= ITEM_REPORT_CHANGE`), so enable checksum mode to assert
    // `checksum_changed()` below.
    let options = LocalCopyOptions::default()
        .checksum(true)
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

    // upstream: generator.c:1942 - position-2 `c` is reserved for `--checksum`
    // mode, so enable it here to exercise the `>f.st......` upstream pattern.
    let options = LocalCopyOptions::default()
        .checksum(true)
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

#[cfg(unix)]
#[test]
fn itemize_existing_root_dir_emits_metadata_reused() {
    // upstream: generator.c:1480-1483 + 582-583 - the transfer-root "." entry is
    // itemized whether or not it changed; under `INFO_GTE(NAME, 2)` (`-vv`) the
    // unchanged root prints `.d ./`. The local-copy engine previously skipped the
    // "." record entirely when the destination root already existed, so the row
    // never appeared under `-vv` while child directories still did. Assert the
    // root frame now emits a `MetadataReused` record for "." so the renderer can
    // surface `.d ./` under `-vv` (and suppress it under `-i`), matching upstream
    // and the other directory rows (regression for upstream `itemize.test`).
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source_dir");
    let destination = temp.path().join("dest_dir");

    fs::create_dir(&source).expect("create source dir");
    fs::create_dir(&destination).expect("create dest dir");
    fs::write(source.join("file.txt"), b"content").expect("write child file");

    // Pin both directory mtimes identical so the root "." is genuinely unchanged.
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source dir mtime");
    set_file_mtime(&destination, timestamp).expect("set dest dir mtime");

    // Trailing slash on the source copies its contents INTO the existing
    // destination, so the transfer root is `dest_dir` itself (relative=None).
    let mut source_with_slash = source.into_os_string();
    source_with_slash.push("/");
    let operands = vec![source_with_slash, destination.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .recursive(true)
        .times(true)
        .collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let root_record = report
        .records()
        .iter()
        .find(|r| r.relative_path() == std::path::Path::new("."))
        .expect("transfer-root \".\" entry must produce an itemize record");

    assert!(!root_record.was_created());
    assert_eq!(root_record.action(), &LocalCopyAction::MetadataReused);
}
