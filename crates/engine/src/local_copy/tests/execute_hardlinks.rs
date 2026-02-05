
#[cfg(unix)]
#[test]
fn execute_with_delay_updates_preserves_hard_links() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");
    let file_a = source_root.join("file-a");
    let file_b = source_root.join("file-b");
    fs::write(&file_a, b"shared").expect("write source file");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .hard_links(true)
        .partial(true)
        .delay_updates(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("file-a");
    let dest_b = dest_root.join("file-b");
    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");

    assert_eq!(metadata_a.ino(), metadata_b.ino());
    assert_eq!(metadata_a.nlink(), 2);
    assert_eq!(metadata_b.nlink(), 2);
    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"shared");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"shared");
    assert!(summary.hard_links_created() >= 1);

    for entry in fs::read_dir(&dest_root).expect("read dest") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with(".rsync-tmp-") && !name.starts_with(".rsync-partial-"),
            "unexpected temporary file left behind: {name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn execute_with_link_dest_uses_reference_inode() {
    use filetime::FileTime;
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("src");
    fs::create_dir_all(&source_dir).expect("create source dir");
    let source_file = source_dir.join("data.txt");
    fs::write(&source_file, b"payload").expect("write source");

    let baseline_dir = temp.path().join("baseline");
    fs::create_dir_all(&baseline_dir).expect("create baseline dir");
    let baseline_file = baseline_dir.join("data.txt");
    fs::write(&baseline_file, b"payload").expect("write baseline");

    let source_metadata = fs::metadata(&source_file).expect("source metadata");
    let source_mtime = source_metadata.modified().expect("source modified time");
    let timestamp = FileTime::from_system_time(source_mtime);
    filetime::set_file_times(&baseline_file, timestamp, timestamp)
        .expect("synchronise baseline timestamps");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create destination dir");

    let operands = vec![
        source_file.into_os_string(),
        dest_dir.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .times(true)
        .extend_link_dests([baseline_dir]);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_file = dest_dir.join("data.txt");
    let dest_metadata = fs::metadata(&dest_file).expect("dest metadata");
    let baseline_metadata = fs::metadata(&baseline_file).expect("baseline metadata");

    assert_eq!(dest_metadata.ino(), baseline_metadata.ino());
    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_detects_hard_links_between_files() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let file_a = source_root.join("file-a.txt");
    let file_b = source_root.join("file-b.txt");
    let file_c = source_root.join("file-c.txt");

    fs::write(&file_a, b"hardlinked content").expect("write file a");
    fs::hard_link(&file_a, &file_b).expect("create hard link b");
    fs::write(&file_c, b"independent content").expect("write file c");

    let source_metadata_a = fs::metadata(&file_a).expect("metadata a");
    let source_metadata_b = fs::metadata(&file_b).expect("metadata b");

    assert_eq!(source_metadata_a.ino(), source_metadata_b.ino());
    assert_eq!(source_metadata_a.nlink(), 2);
    assert_eq!(source_metadata_b.nlink(), 2);

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("file-a.txt");
    let dest_b = dest_root.join("file-b.txt");
    let dest_c = dest_root.join("file-c.txt");

    let dest_metadata_a = fs::metadata(&dest_a).expect("dest metadata a");
    let dest_metadata_b = fs::metadata(&dest_b).expect("dest metadata b");
    let dest_metadata_c = fs::metadata(&dest_c).expect("dest metadata c");

    assert_eq!(dest_metadata_a.ino(), dest_metadata_b.ino());
    assert_ne!(dest_metadata_a.ino(), dest_metadata_c.ino());
    assert_eq!(dest_metadata_a.nlink(), 2);
    assert_eq!(dest_metadata_b.nlink(), 2);
    assert_eq!(dest_metadata_c.nlink(), 1);

    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"hardlinked content");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"hardlinked content");
    assert_eq!(fs::read(&dest_c).expect("read dest c"), b"independent content");

    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_preserves_multiple_hard_links_to_same_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let original = source_root.join("original.txt");
    let link1 = source_root.join("link1.txt");
    let link2 = source_root.join("link2.txt");
    let link3 = source_root.join("link3.txt");

    fs::write(&original, b"shared data").expect("write original");
    fs::hard_link(&original, &link1).expect("create link1");
    fs::hard_link(&original, &link2).expect("create link2");
    fs::hard_link(&original, &link3).expect("create link3");

    let source_metadata = fs::metadata(&original).expect("source metadata");
    assert_eq!(source_metadata.nlink(), 4);

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_original = dest_root.join("original.txt");
    let dest_link1 = dest_root.join("link1.txt");
    let dest_link2 = dest_root.join("link2.txt");
    let dest_link3 = dest_root.join("link3.txt");

    let dest_metadata_orig = fs::metadata(&dest_original).expect("dest metadata orig");
    let dest_metadata_link1 = fs::metadata(&dest_link1).expect("dest metadata link1");
    let dest_metadata_link2 = fs::metadata(&dest_link2).expect("dest metadata link2");
    let dest_metadata_link3 = fs::metadata(&dest_link3).expect("dest metadata link3");

    let dest_inode = dest_metadata_orig.ino();
    assert_eq!(dest_metadata_link1.ino(), dest_inode);
    assert_eq!(dest_metadata_link2.ino(), dest_inode);
    assert_eq!(dest_metadata_link3.ino(), dest_inode);

    assert_eq!(dest_metadata_orig.nlink(), 4);
    assert_eq!(dest_metadata_link1.nlink(), 4);
    assert_eq!(dest_metadata_link2.nlink(), 4);
    assert_eq!(dest_metadata_link3.nlink(), 4);

    assert_eq!(fs::read(&dest_original).expect("read orig"), b"shared data");
    assert_eq!(fs::read(&dest_link1).expect("read link1"), b"shared data");
    assert_eq!(fs::read(&dest_link2).expect("read link2"), b"shared data");
    assert_eq!(fs::read(&dest_link3).expect("read link3"), b"shared data");

    assert!(summary.hard_links_created() >= 3);
}

#[cfg(unix)]
#[test]
fn execute_hardlink_tracking_across_subdirectories() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let subdir1 = source_root.join("dir1");
    let subdir2 = source_root.join("dir2");
    fs::create_dir_all(&subdir1).expect("create subdir1");
    fs::create_dir_all(&subdir2).expect("create subdir2");

    let file1 = subdir1.join("file.txt");
    let file2 = subdir2.join("linked.txt");
    let file3 = source_root.join("root-link.txt");

    fs::write(&file1, b"linked across directories").expect("write file1");
    fs::hard_link(&file1, &file2).expect("create hard link in dir2");
    fs::hard_link(&file1, &file3).expect("create hard link in root");

    let source_metadata = fs::metadata(&file1).expect("source metadata");
    assert_eq!(source_metadata.nlink(), 3);

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_file1 = dest_root.join("dir1").join("file.txt");
    let dest_file2 = dest_root.join("dir2").join("linked.txt");
    let dest_file3 = dest_root.join("root-link.txt");

    let dest_metadata1 = fs::metadata(&dest_file1).expect("dest metadata 1");
    let dest_metadata2 = fs::metadata(&dest_file2).expect("dest metadata 2");
    let dest_metadata3 = fs::metadata(&dest_file3).expect("dest metadata 3");

    assert_eq!(dest_metadata1.ino(), dest_metadata2.ino());
    assert_eq!(dest_metadata1.ino(), dest_metadata3.ino());
    assert_eq!(dest_metadata1.nlink(), 3);
    assert_eq!(dest_metadata2.nlink(), 3);
    assert_eq!(dest_metadata3.nlink(), 3);

    assert_eq!(
        fs::read(&dest_file1).expect("read dest1"),
        b"linked across directories"
    );
    assert_eq!(
        fs::read(&dest_file2).expect("read dest2"),
        b"linked across directories"
    );
    assert_eq!(
        fs::read(&dest_file3).expect("read dest3"),
        b"linked across directories"
    );

    assert!(summary.hard_links_created() >= 2);
}

#[cfg(unix)]
#[test]
fn execute_without_hard_links_option_copies_separately() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let file_a = source_root.join("file-a.txt");
    let file_b = source_root.join("file-b.txt");

    fs::write(&file_a, b"content").expect("write file a");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let source_metadata_a = fs::metadata(&file_a).expect("source metadata a");
    let source_metadata_b = fs::metadata(&file_b).expect("source metadata b");
    assert_eq!(source_metadata_a.ino(), source_metadata_b.ino());

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(false);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("file-a.txt");
    let dest_b = dest_root.join("file-b.txt");

    let dest_metadata_a = fs::metadata(&dest_a).expect("dest metadata a");
    let dest_metadata_b = fs::metadata(&dest_b).expect("dest metadata b");

    assert_ne!(
        dest_metadata_a.ino(),
        dest_metadata_b.ino(),
        "files should have different inodes when hard_links is disabled"
    );
    assert_eq!(dest_metadata_a.nlink(), 1);
    assert_eq!(dest_metadata_b.nlink(), 1);

    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"content");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"content");

    assert_eq!(summary.hard_links_created(), 0);
}

#[cfg(unix)]
#[test]
fn execute_hardlink_with_partial_and_delay_updates() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let file_a = source_root.join("alpha.txt");
    let file_b = source_root.join("beta.txt");
    let file_c = source_root.join("gamma.txt");

    fs::write(&file_a, b"first hardlink set").expect("write file a");
    fs::hard_link(&file_a, &file_b).expect("create hard link b");
    fs::write(&file_c, b"independent").expect("write file c");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default()
        .hard_links(true)
        .partial(true)
        .delay_updates(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_a = dest_root.join("alpha.txt");
    let dest_b = dest_root.join("beta.txt");
    let dest_c = dest_root.join("gamma.txt");

    let metadata_a = fs::metadata(&dest_a).expect("metadata a");
    let metadata_b = fs::metadata(&dest_b).expect("metadata b");
    let metadata_c = fs::metadata(&dest_c).expect("metadata c");

    assert_eq!(metadata_a.ino(), metadata_b.ino());
    assert_ne!(metadata_a.ino(), metadata_c.ino());
    assert_eq!(metadata_a.nlink(), 2);
    assert_eq!(metadata_b.nlink(), 2);
    assert_eq!(metadata_c.nlink(), 1);

    assert_eq!(fs::read(&dest_a).expect("read dest a"), b"first hardlink set");
    assert_eq!(fs::read(&dest_b).expect("read dest b"), b"first hardlink set");
    assert_eq!(fs::read(&dest_c).expect("read dest c"), b"independent");

    assert!(summary.hard_links_created() >= 1);

    for entry in fs::read_dir(&dest_root).expect("read dest") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        assert!(
            !name_str.starts_with(".rsync-tmp-") && !name_str.starts_with(".rsync-partial-"),
            "unexpected temporary file left behind: {name_str}"
        );
    }
}

#[cfg(unix)]
#[test]
fn execute_hardlink_tracking_table_consistency() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let file1 = source_root.join("first.txt");
    let file2 = source_root.join("second.txt");
    let file3 = source_root.join("third.txt");

    fs::write(&file1, b"group1").expect("write file1");
    fs::hard_link(&file1, &file2).expect("link to file2");

    fs::write(&file3, b"group2").expect("write file3");
    let file4 = source_root.join("fourth.txt");
    fs::hard_link(&file3, &file4).expect("link to file4");

    let source_meta1 = fs::metadata(&file1).expect("source meta1");
    let source_meta2 = fs::metadata(&file2).expect("source meta2");
    let source_meta3 = fs::metadata(&file3).expect("source meta3");
    let source_meta4 = fs::metadata(&file4).expect("source meta4");

    assert_eq!(source_meta1.ino(), source_meta2.ino());
    assert_eq!(source_meta3.ino(), source_meta4.ino());
    assert_ne!(source_meta1.ino(), source_meta3.ino());

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest1 = dest_root.join("first.txt");
    let dest2 = dest_root.join("second.txt");
    let dest3 = dest_root.join("third.txt");
    let dest4 = dest_root.join("fourth.txt");

    let dest_meta1 = fs::metadata(&dest1).expect("dest meta1");
    let dest_meta2 = fs::metadata(&dest2).expect("dest meta2");
    let dest_meta3 = fs::metadata(&dest3).expect("dest meta3");
    let dest_meta4 = fs::metadata(&dest4).expect("dest meta4");

    assert_eq!(dest_meta1.ino(), dest_meta2.ino(), "first group should share inode");
    assert_eq!(dest_meta3.ino(), dest_meta4.ino(), "second group should share inode");
    assert_ne!(
        dest_meta1.ino(),
        dest_meta3.ino(),
        "different groups should have different inodes"
    );

    assert_eq!(dest_meta1.nlink(), 2);
    assert_eq!(dest_meta2.nlink(), 2);
    assert_eq!(dest_meta3.nlink(), 2);
    assert_eq!(dest_meta4.nlink(), 2);

    assert_eq!(fs::read(&dest1).expect("read dest1"), b"group1");
    assert_eq!(fs::read(&dest2).expect("read dest2"), b"group1");
    assert_eq!(fs::read(&dest3).expect("read dest3"), b"group2");
    assert_eq!(fs::read(&dest4).expect("read dest4"), b"group2");

    assert!(summary.hard_links_created() >= 2);
}

#[cfg(unix)]
#[test]
fn execute_hardlink_with_existing_destination() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let file_a = source_root.join("file-a.txt");
    let file_b = source_root.join("file-b.txt");

    fs::write(&file_a, b"new content").expect("write file a");
    fs::hard_link(&file_a, &file_b).expect("create hard link");

    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let dest_a = dest_root.join("file-a.txt");
    let dest_b = dest_root.join("file-b.txt");
    fs::write(&dest_a, b"old content a").expect("write old dest a");
    fs::write(&dest_b, b"old content b").expect("write old dest b");

    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true).ignore_times(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    // Files are created in dest/source/ subdirectory
    let dest_a_actual = dest_root.join("source/file-a.txt");
    let dest_b_actual = dest_root.join("source/file-b.txt");
    let dest_metadata_a = fs::metadata(&dest_a_actual).expect("dest metadata a");
    let dest_metadata_b = fs::metadata(&dest_b_actual).expect("dest metadata b");

    assert_eq!(dest_metadata_a.ino(), dest_metadata_b.ino());
    assert_eq!(dest_metadata_a.nlink(), 2);
    assert_eq!(dest_metadata_b.nlink(), 2);

    assert_eq!(fs::read(&dest_a_actual).expect("read dest a"), b"new content");
    assert_eq!(fs::read(&dest_b_actual).expect("read dest b"), b"new content");

    assert!(summary.hard_links_created() >= 1);
}

#[cfg(unix)]
#[test]
fn execute_hardlink_preserves_first_occurrence_then_links() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    fs::create_dir_all(&source_root).expect("create source root");

    let original = source_root.join("aaa-first.txt");
    let link1 = source_root.join("bbb-second.txt");
    let link2 = source_root.join("ccc-third.txt");

    fs::write(&original, b"data").expect("write original");
    fs::hard_link(&original, &link1).expect("create link1");
    fs::hard_link(&original, &link2).expect("create link2");

    let dest_root = temp.path().join("dest");
    let operands = vec![
        source_root.into_os_string(),
        dest_root.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let options = LocalCopyOptions::default().hard_links(true);
    let summary = plan
        .execute_with_options(LocalCopyExecution::Apply, options)
        .expect("copy succeeds");

    let dest_orig = dest_root.join("aaa-first.txt");
    let dest_link1 = dest_root.join("bbb-second.txt");
    let dest_link2 = dest_root.join("ccc-third.txt");

    assert!(dest_orig.exists());
    assert!(dest_link1.exists());
    assert!(dest_link2.exists());

    let meta_orig = fs::metadata(&dest_orig).expect("meta orig");
    let meta_link1 = fs::metadata(&dest_link1).expect("meta link1");
    let meta_link2 = fs::metadata(&dest_link2).expect("meta link2");

    assert_eq!(meta_orig.ino(), meta_link1.ino());
    assert_eq!(meta_orig.ino(), meta_link2.ino());
    assert_eq!(meta_orig.nlink(), 3);

    assert!(summary.hard_links_created() >= 2);
}
