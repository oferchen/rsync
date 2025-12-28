
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
