
#[cfg(unix)]
#[test]
fn execute_with_sparse_enabled_creates_holes() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("sparse.bin");
    let mut source_file = fs::File::create(&source).expect("create source");
    source_file.write_all(&[0xAA]).expect("write leading byte");
    source_file
        .seek(SeekFrom::Start(2 * 1024 * 1024))
        .expect("seek to create hole");
    source_file.write_all(&[0xBB]).expect("write trailing byte");
    source_file.set_len(4 * 1024 * 1024).expect("extend source");

    let dense_dest = temp.path().join("dense.bin");
    let sparse_dest = temp.path().join("sparse-copy.bin");

    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    let plan_sparse = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan sparse");
    plan_sparse
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true),
        )
        .expect("sparse copy succeeds");

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert!(sparse_meta.blocks() < dense_meta.blocks());
}

#[cfg(unix)]
#[test]
fn execute_with_sparse_enabled_counts_literal_data() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("zeros.bin");
    let file = fs::File::create(&source).expect("create source");
    file.set_len(1_048_576).expect("extend source");

    let destination = temp.path().join("dest.bin");
    let operands = vec![
        source.into_os_string(),
        destination.into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true),
        )
        .expect("sparse copy succeeds");

    assert_eq!(summary.bytes_copied(), 1_048_576);
    assert_eq!(summary.matched_bytes(), 0);
}

#[cfg(unix)]
#[test]
fn execute_delta_with_sparse_counts_zero_literal_data() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&dest_root).expect("create dest root");

    let source_path = source_root.join("file.bin");
    let dest_path = dest_root.join("file.bin");

    let prefix = vec![b'A'; 700];
    let zeros = vec![0u8; 700];
    let previous = vec![b'X'; zeros.len()];
    let prefix_len = prefix.len() as u64;
    let literal_len = zeros.len() as u64;

    let mut initial = Vec::with_capacity(prefix.len() + previous.len());
    initial.extend_from_slice(&prefix);
    initial.extend_from_slice(&previous);
    fs::write(&dest_path, &initial).expect("write initial destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set destination mtime");

    let mut updated = Vec::with_capacity(prefix.len() + zeros.len());
    updated.extend_from_slice(&prefix);
    updated.extend_from_slice(&zeros);
    fs::write(&source_path, &updated).expect("write updated source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("set source mtime");

    let operands = vec![
        source_path.clone().into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(false)
                .sparse(true),
        )
        .expect("delta sparse copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), literal_len);
    assert_eq!(summary.matched_bytes(), prefix_len);
    assert_eq!(fs::read(&dest_path).expect("read destination"), updated);
}

#[cfg(unix)]
#[test]
fn execute_without_inplace_replaces_destination_file() {
    use std::os::unix::fs::MetadataExt;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"updated").expect("write source");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("target.txt");
    fs::write(&destination, b"original").expect("write destination");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan.execute().expect("copy succeeds");
    assert_eq!(summary.files_copied(), 1);

    let updated_metadata = fs::metadata(&destination).expect("destination metadata");
    assert_ne!(updated_metadata.ino(), original_inode);
    assert_eq!(
        fs::read(&destination).expect("read destination"),
        b"updated"
    );

    let mut entries = fs::read_dir(&dest_dir).expect("list dest dir");
    assert!(entries.all(|entry| {
        let name = entry.expect("dir entry").file_name();
        !name.to_string_lossy().starts_with(".rsync-tmp-")
    }));
}

#[cfg(unix)]
#[test]
fn execute_inplace_succeeds_with_read_only_directory() {
    use rustix::fs::{Mode, chmod};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"replacement").expect("write source");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("target.txt");
    fs::write(&destination, b"original").expect("write destination");
    fs::set_permissions(&destination, PermissionsExt::from_mode(0o644))
        .expect("make destination writable");

    let original_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();

    let readonly = Mode::from_bits_truncate(0o555);
    chmod(&dest_dir, readonly).expect("restrict directory permissions");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("in-place copy succeeds");

    let contents = fs::read(&destination).expect("read destination");
    assert_eq!(contents, b"replacement");
    assert_eq!(summary.files_copied(), 1);

    let updated_inode = fs::metadata(&destination)
        .expect("destination metadata")
        .ino();
    assert_eq!(updated_inode, original_inode);

    let restore = Mode::from_bits_truncate(0o755);
    chmod(&dest_dir, restore).expect("restore directory permissions");
}
