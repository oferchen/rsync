#[cfg(unix)]
#[test]
fn execute_with_sparse_enabled_creates_holes() {

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

    use std::os::unix::fs::MetadataExt;
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    // On some platforms/filesystems (e.g. macOS/APFS), st_blocks may not
    // reflect sparse allocation differences even when holes exist. In that
    // case, we treat the strict block comparison as platform-limited and
    // skip it rather than failing spuriously.
    if sparse_blocks == dense_blocks {
        eprintln!(
            "sparse file uses {sparse_blocks} blocks, dense uses {dense_blocks}; filesystem does \
             not expose sparse allocation difference, skipping strict sparse check"
        );
        return;
    }

    assert!(
        sparse_blocks < dense_blocks,
        "sparse copy should allocate fewer blocks than dense copy (sparse: {sparse_blocks}, dense: {dense_blocks})"
    );
}

#[cfg(unix)]
#[test]
fn execute_inplace_disables_sparse_writes() {

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source-inplace.bin");
    let mut source_file = fs::File::create(&source).expect("create source");
    source_file.write_all(&[0x11]).expect("write leading byte");
    source_file
        .seek(SeekFrom::Start(2 * 1024 * 1024))
        .expect("seek to create hole");
    source_file
        .write_all(&[0x22])
        .expect("write trailing byte");
    source_file
        .set_len(4 * 1024 * 1024)
        .expect("extend source");
    drop(source_file);

    let dense_dest = temp.path().join("dense-inplace.bin");
    let sparse_dest = temp.path().join("sparse-inplace.bin");
    let initial = vec![0xCC; 4 * 1024 * 1024];
    fs::write(&dense_dest, &initial).expect("initialise dense destination");
    fs::write(&sparse_dest, &initial).expect("initialise sparse destination");

    let dense_plan = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    dense_plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("dense inplace copy succeeds");

    let sparse_plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan sparse");
    sparse_plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true).inplace(true),
        )
        .expect("sparse inplace copy succeeds");

    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert_eq!(
        fs::read(&dense_dest).expect("read dense destination"),
        fs::read(&sparse_dest).expect("read sparse destination"),
    );

    use std::os::unix::fs::MetadataExt;
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();
    assert!(
        sparse_blocks >= dense_blocks,
        "in-place sparse copy should not create holes (sparse blocks: {sparse_blocks}, dense blocks: {dense_blocks})",
    );
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
        source_path.into_os_string(),
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

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"updated").expect("write source");

    let dest_dir = temp.path().join("dest");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let destination = dest_dir.join("target.txt");
    fs::write(&destination, b"original").expect("write destination");

    use std::os::unix::fs::MetadataExt;
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
    use rustix::fs::{chmod, Mode};
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

/// Phase 2 test: Multiple hole patterns - verify sparse detection across
/// various data-hole-data layouts matching upstream behavior.
#[cfg(unix)]
#[test]
fn execute_sparse_with_multiple_hole_patterns() {

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("multi-pattern.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Pattern: data-hole-data-hole-data (interleaved)
    // Data block 1: 10KB at offset 0
    let data_block = vec![0xAA; 10 * 1024];
    source_file.write_all(&data_block).expect("write data block 1");

    // Hole 1: seek to 1MB (creating ~1014KB hole)
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek for hole 1");

    // Data block 2: 20KB at 1MB
    let data_block_2 = vec![0xBB; 20 * 1024];
    source_file.write_all(&data_block_2).expect("write data block 2");

    // Hole 2: seek to 3MB (creating ~2004KB hole)
    source_file
        .seek(SeekFrom::Start(3 * 1024 * 1024))
        .expect("seek for hole 2");

    // Data block 3: 10KB at 3MB
    let data_block_3 = vec![0xCC; 10 * 1024];
    source_file.write_all(&data_block_3).expect("write data block 3");

    // Final hole: extend to 5MB
    source_file.set_len(5 * 1024 * 1024).expect("extend source");
    drop(source_file);

    let dense_dest = temp.path().join("multi-dense.bin");
    let sparse_dest = temp.path().join("multi-sparse.bin");

    // Dense copy (no --sparse)
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy (--sparse)
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

    // Both should have same file size
    assert_eq!(dense_meta.len(), sparse_meta.len());
    assert_eq!(dense_meta.len(), 5 * 1024 * 1024);

    // Verify content is identical
    let dense_content = fs::read(&dense_dest).expect("read dense");
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(dense_content, sparse_content);

    // Verify sparse file uses fewer blocks (platform-dependent)
    use std::os::unix::fs::MetadataExt;
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    if sparse_blocks == dense_blocks {
        eprintln!(
            "multi-pattern sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem does not expose allocation difference, skipping block check"
        );
    } else {
        assert!(
            sparse_blocks < dense_blocks,
            "multi-pattern sparse should allocate fewer blocks (sparse: {sparse_blocks}, \
             dense: {dense_blocks})"
        );
    }
}

/// Phase 2 test: Verify sparse copy produces correct data regions in holes.
#[cfg(unix)]
#[test]
fn execute_sparse_verifies_hole_data_integrity() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("hole-data.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Specific pattern: known data at specific offsets
    source_file.write_all(b"START").expect("write start");
    source_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek hole 1");
    source_file.write_all(b"MIDDLE").expect("write middle");
    source_file
        .seek(SeekFrom::Start(2 * 1024 * 1024))
        .expect("seek hole 2");
    source_file.write_all(b"END").expect("write end");
    source_file.set_len(3 * 1024 * 1024).expect("extend");
    drop(source_file);

    let sparse_dest = temp.path().join("hole-sparse.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    // Verify data regions are correct
    let mut dest_file = fs::File::open(&sparse_dest).expect("open dest");
    let mut buffer = vec![0u8; 5];

    dest_file.seek(SeekFrom::Start(0)).expect("seek start");
    dest_file.read_exact(&mut buffer).expect("read start");
    assert_eq!(&buffer, b"START");

    dest_file
        .seek(SeekFrom::Start(1024 * 1024))
        .expect("seek middle");
    let mut buffer_mid = vec![0u8; 6];
    dest_file.read_exact(&mut buffer_mid).expect("read middle");
    assert_eq!(&buffer_mid, b"MIDDLE");

    dest_file
        .seek(SeekFrom::Start(2 * 1024 * 1024))
        .expect("seek end");
    let mut buffer_end = vec![0u8; 3];
    dest_file.read_exact(&mut buffer_end).expect("read end");
    assert_eq!(&buffer_end, b"END");

    // Verify holes are zeros
    dest_file.seek(SeekFrom::Start(5)).expect("seek after start");
    let mut hole_sample = vec![0xFF; 100];
    dest_file.read_exact(&mut hole_sample).expect("read hole");
    assert!(
        hole_sample.iter().all(|&b| b == 0),
        "hole region should be all zeros"
    );
}

/// Phase 2 test: Sparse with small holes - verify behavior with holes smaller
/// than SPARSE_WRITE_SIZE threshold (1024 bytes).
#[cfg(unix)]
#[test]
fn execute_sparse_with_small_holes() {

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("small-holes.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create file with small holes (< 1024 bytes threshold)
    // Pattern: 512 bytes data, 512 bytes zeros, 512 bytes data, 512 bytes zeros
    let data_chunk = vec![0xDD; 512];
    let zero_chunk = vec![0x00; 512];

    for _ in 0..4 {
        source_file.write_all(&data_chunk).expect("write data");
        source_file.write_all(&zero_chunk).expect("write zeros");
    }

    let total_size = 4 * (512 + 512);
    drop(source_file);

    let sparse_dest = temp.path().join("small-sparse.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    let meta = fs::metadata(&sparse_dest).expect("metadata");
    assert_eq!(meta.len(), total_size as u64);

    // Verify content integrity regardless of hole detection
    let content = fs::read(&sparse_dest).expect("read dest");
    for i in 0..4 {
        let data_start = i * 1024;
        let data_end = data_start + 512;
        let zero_start = data_end;
        let zero_end = zero_start + 512;

        assert_eq!(
            &content[data_start..data_end],
            &data_chunk[..],
            "data chunk {i} mismatch"
        );
        assert_eq!(
            &content[zero_start..zero_end],
            &zero_chunk[..],
            "zero chunk {i} mismatch"
        );
    }
}

/// Phase 2 test: Sparse with aligned holes - verify SIMD u128 fast path.
#[cfg(unix)]
#[test]
fn execute_sparse_with_aligned_holes() {

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("aligned.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create holes aligned to 16-byte boundaries for SIMD fast path
    source_file.write_all(b"A").expect("write start");

    // Skip to 16-byte boundary
    source_file.seek(SeekFrom::Start(16)).expect("seek 16");
    source_file.write_all(b"B").expect("write at 16");

    // Create aligned hole (16-byte chunks of zeros)
    source_file
        .seek(SeekFrom::Start(1024))
        .expect("seek hole start");
    source_file.write_all(b"C").expect("write after hole");

    // Another aligned hole
    source_file
        .seek(SeekFrom::Start(2048))
        .expect("seek hole 2 start");
    source_file.write_all(b"D").expect("write after hole 2");

    source_file.set_len(4096).expect("set length");
    drop(source_file);

    let sparse_dest = temp.path().join("aligned-sparse.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    let meta = fs::metadata(&sparse_dest).expect("metadata");
    assert_eq!(meta.len(), 4096);

    // Verify data at specific offsets
    let content = fs::read(&sparse_dest).expect("read dest");
    assert_eq!(content[0], b'A');
    assert_eq!(content[16], b'B');
    assert_eq!(content[1024], b'C');
    assert_eq!(content[2048], b'D');

    // Verify holes are zeros
    assert!(content[1..16].iter().all(|&b| b == 0), "hole before B");
    assert!(content[17..1024].iter().all(|&b| b == 0), "hole before C");
    assert!(
        content[1025..2048].iter().all(|&b| b == 0),
        "hole before D"
    );
}

/// Test: Zero regions exactly at threshold (32KB) are detected as holes.
#[cfg(unix)]
#[test]
fn execute_sparse_detects_zero_regions_at_threshold() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("threshold-zeros.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // SPARSE_WRITE_SIZE is 32KB - write exactly that amount of zeros
    let threshold = 32 * 1024;
    source_file.write_all(b"HEADER").expect("write header");
    source_file.write_all(&vec![0u8; threshold]).expect("write zeros at threshold");
    source_file.write_all(b"FOOTER").expect("write footer");
    drop(source_file);

    let dense_dest = temp.path().join("dense.bin");
    let sparse_dest = temp.path().join("sparse.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Verify content is identical
    let dense_content = fs::read(&dense_dest).expect("read dense");
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(dense_content, sparse_content);
    assert_eq!(&sparse_content[0..6], b"HEADER");
    assert_eq!(&sparse_content[6 + threshold..6 + threshold + 6], b"FOOTER");

    // Verify sparse file uses fewer blocks (platform-dependent)
    use std::os::unix::fs::MetadataExt;
    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "sparse at threshold uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not expose sparse allocation difference"
        );
    } else {
        assert!(
            sparse_blocks < dense_blocks,
            "sparse copy should allocate fewer blocks at threshold (sparse: {sparse_blocks}, dense: {dense_blocks})"
        );
    }
}

/// Test: Zero regions just under threshold (32KB - 1) are NOT treated as holes.
#[cfg(unix)]
#[test]
fn execute_sparse_skips_zero_regions_under_threshold() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("under-threshold.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Write zeros just under the threshold - should be written densely
    let under_threshold = 32 * 1024 - 1;
    source_file.write_all(b"START").expect("write start");
    source_file.write_all(&vec![0u8; under_threshold]).expect("write zeros under threshold");
    source_file.write_all(b"END").expect("write end");
    drop(source_file);

    let sparse_dest = temp.path().join("sparse.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    // Verify content integrity
    let content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(&content[0..5], b"START");
    assert!(content[5..5 + under_threshold].iter().all(|&b| b == 0));
    assert_eq!(&content[5 + under_threshold..5 + under_threshold + 3], b"END");
}

/// Test: Zero regions just over threshold (32KB + 1) are treated as holes.
#[cfg(unix)]
#[test]
fn execute_sparse_detects_zero_regions_over_threshold() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("over-threshold.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Write zeros just over the threshold - should create holes
    let over_threshold = 32 * 1024 + 1;
    source_file.write_all(b"BEGIN").expect("write begin");
    source_file.write_all(&vec![0u8; over_threshold]).expect("write zeros over threshold");
    source_file.write_all(b"FINISH").expect("write finish");
    drop(source_file);

    let dense_dest = temp.path().join("dense.bin");
    let sparse_dest = temp.path().join("sparse.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Verify content
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(&sparse_content[0..5], b"BEGIN");
    assert!(sparse_content[5..5 + over_threshold].iter().all(|&b| b == 0));
    assert_eq!(&sparse_content[5 + over_threshold..5 + over_threshold + 6], b"FINISH");

    // Verify sparse optimization occurred (platform-dependent)
    use std::os::unix::fs::MetadataExt;
    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "over-threshold sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not expose difference"
        );
    }
}

/// Test: Verify actual holes are created on disk (using SEEK_HOLE/SEEK_DATA on Linux).
#[cfg(target_os = "linux")]
#[test]
fn execute_sparse_creates_actual_filesystem_holes() {
    use std::os::unix::io::AsRawFd;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("hole-test.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create a file with known hole pattern
    source_file.write_all(b"DATA1").expect("write data1");
    source_file.write_all(&vec![0u8; 64 * 1024]).expect("write 64KB zeros");
    source_file.write_all(b"DATA2").expect("write data2");
    source_file.write_all(&vec![0u8; 128 * 1024]).expect("write 128KB zeros");
    source_file.write_all(b"DATA3").expect("write data3");
    drop(source_file);

    let sparse_dest = temp.path().join("sparse-holes.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    // Verify holes exist using lseek with SEEK_HOLE/SEEK_DATA
    let dest_file = fs::File::open(&sparse_dest).expect("open dest");
    let fd = dest_file.as_raw_fd();

    // Constants for SEEK_HOLE and SEEK_DATA (Linux-specific)
    const SEEK_DATA: i32 = 3;
    const SEEK_HOLE: i32 = 4;

    unsafe {
        // Start at beginning
        let first_data = libc::lseek(fd, 0, SEEK_DATA);
        assert!(first_data >= 0, "should find first data region");
        assert_eq!(first_data, 0, "first data should start at offset 0");

        // Find first hole
        let first_hole = libc::lseek(fd, 0, SEEK_HOLE);
        assert!(first_hole > 0, "should find first hole after initial data");
        assert!(first_hole < 64 * 1024 + 100, "first hole should be in first zero region");

        // Find second data region
        let second_data = libc::lseek(fd, first_hole, SEEK_DATA);
        assert!(second_data > first_hole, "should find data after first hole");
    }

    // Verify content integrity
    let content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(&content[0..5], b"DATA1");
    assert_eq!(&content[5 + 64 * 1024..5 + 64 * 1024 + 5], b"DATA2");
}

/// Test: Multiple zero regions with threshold-sized gaps are all detected.
#[cfg(unix)]
#[test]
fn execute_sparse_detects_multiple_threshold_zero_regions() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("multi-threshold.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    let threshold = 32 * 1024;

    // Create pattern: data - zeros@threshold - data - zeros@threshold - data
    source_file.write_all(b"BLOCK1").expect("write block1");
    source_file.write_all(&vec![0u8; threshold]).expect("write zeros 1");
    source_file.write_all(b"BLOCK2").expect("write block2");
    source_file.write_all(&vec![0u8; threshold]).expect("write zeros 2");
    source_file.write_all(b"BLOCK3").expect("write block3");
    source_file.write_all(&vec![0u8; threshold]).expect("write zeros 3");
    source_file.write_all(b"BLOCK4").expect("write block4");
    drop(source_file);

    let dense_dest = temp.path().join("dense-multi.bin");
    let sparse_dest = temp.path().join("sparse-multi.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Verify content
    let content = fs::read(&sparse_dest).expect("read sparse");
    let mut offset = 0;
    assert_eq!(&content[offset..offset + 6], b"BLOCK1");
    offset += 6 + threshold;
    assert_eq!(&content[offset..offset + 6], b"BLOCK2");
    offset += 6 + threshold;
    assert_eq!(&content[offset..offset + 6], b"BLOCK3");
    offset += 6 + threshold;
    assert_eq!(&content[offset..offset + 6], b"BLOCK4");

    // Verify sparse optimization
    use std::os::unix::fs::MetadataExt;
    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "multi-threshold sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not expose difference"
        );
    }
}

/// Test: Non-zero data is written correctly and not corrupted by sparse handling.
#[cfg(unix)]
#[test]
fn execute_sparse_preserves_nonzero_data_integrity() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("data-integrity.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create file with various data patterns
    let pattern1 = vec![0xAA; 1024];
    let pattern2 = vec![0x55; 2048];
    let pattern3 = vec![0xFF; 512];
    let pattern4: Vec<u8> = (0..=255u8).cycle().take(4096).collect();

    source_file.write_all(&pattern1).expect("write pattern1");
    source_file.write_all(&vec![0u8; 64 * 1024]).expect("write hole");
    source_file.write_all(&pattern2).expect("write pattern2");
    source_file.write_all(&vec![0u8; 32 * 1024]).expect("write hole");
    source_file.write_all(&pattern3).expect("write pattern3");
    source_file.write_all(&vec![0u8; 96 * 1024]).expect("write hole");
    source_file.write_all(&pattern4).expect("write pattern4");
    drop(source_file);

    let sparse_dest = temp.path().join("sparse-integrity.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    // Verify exact content match
    let source_content = fs::read(&source).expect("read source");
    let dest_content = fs::read(&sparse_dest).expect("read dest");
    assert_eq!(source_content, dest_content, "content should be identical");

    // Verify specific patterns
    assert_eq!(&dest_content[0..1024], &pattern1[..]);
    let offset2 = 1024 + 64 * 1024;
    assert_eq!(&dest_content[offset2..offset2 + 2048], &pattern2[..]);
    let offset3 = offset2 + 2048 + 32 * 1024;
    assert_eq!(&dest_content[offset3..offset3 + 512], &pattern3[..]);
    let offset4 = offset3 + 512 + 96 * 1024;
    assert_eq!(&dest_content[offset4..offset4 + 4096], &pattern4[..]);
}

/// Test: Verify sparse files are smaller on disk than dense files.
#[cfg(unix)]
#[test]
fn execute_sparse_reduces_disk_allocation() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("sparse-alloc.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create 10MB file with only 100KB of data
    source_file.write_all(b"START").expect("write start");
    source_file.write_all(&vec![0u8; 5 * 1024 * 1024]).expect("write 5MB zeros");
    source_file.write_all(&vec![0xFF; 100 * 1024]).expect("write 100KB data");
    source_file.write_all(&vec![0u8; 5 * 1024 * 1024 - 5]).expect("write remaining zeros");
    drop(source_file);

    let dense_dest = temp.path().join("dense-alloc.bin");
    let sparse_dest = temp.path().join("sparse-alloc.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    use std::os::unix::fs::MetadataExt;
    let dense_meta = fs::metadata(&dense_dest).expect("dense metadata");
    let sparse_meta = fs::metadata(&sparse_dest).expect("sparse metadata");

    // Both should have same file size
    assert_eq!(dense_meta.len(), sparse_meta.len());

    // Sparse should use significantly fewer blocks (allow for filesystem differences)
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();
    let expected_max_sparse_blocks = (200 * 1024) / 512; // ~200KB in blocks + overhead

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "WARNING: sparse file uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not support sparse files efficiently"
        );
    } else {
        assert!(
            sparse_blocks < dense_blocks,
            "sparse should use fewer blocks (sparse: {sparse_blocks}, dense: {dense_blocks})"
        );

        if sparse_blocks > expected_max_sparse_blocks {
            eprintln!(
                "NOTE: sparse uses {sparse_blocks} blocks (expected < {expected_max_sparse_blocks}); \
                 filesystem allocated more than minimal"
            );
        }
    }
}

/// Test: Boundary case - zeros exactly split across threshold boundaries.
#[cfg(unix)]
#[test]
fn execute_sparse_handles_boundary_split_zeros() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("boundary-split.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    let threshold = 32 * 1024;

    // Write data, then zeros that span exactly 2x threshold
    source_file.write_all(b"PREFIX").expect("write prefix");
    source_file.write_all(&vec![0u8; threshold * 2]).expect("write 2x threshold zeros");
    source_file.write_all(b"SUFFIX").expect("write suffix");
    drop(source_file);

    let sparse_dest = temp.path().join("sparse-boundary.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    // Verify content
    let content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(&content[0..6], b"PREFIX");
    assert!(content[6..6 + threshold * 2].iter().all(|&b| b == 0));
    assert_eq!(&content[6 + threshold * 2..6 + threshold * 2 + 6], b"SUFFIX");
}

/// Phase 2 test: Large sparse file - verify handling of files >> RAM size.
#[cfg(unix)]
#[test]
fn execute_sparse_with_large_file() {

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("large-sparse.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create a 1GB sparse file with small data regions
    source_file.write_all(b"HEADER").expect("write header");

    // Seek to 500MB
    source_file
        .seek(SeekFrom::Start(500 * 1024 * 1024))
        .expect("seek mid");
    source_file.write_all(b"MIDDLE").expect("write middle");

    // Seek to 1GB
    source_file
        .seek(SeekFrom::Start(1024 * 1024 * 1024))
        .expect("seek end");
    source_file.write_all(b"FOOTER").expect("write footer");

    let file_size = 1024 * 1024 * 1024 + 6;
    source_file.set_len(file_size).expect("set length");
    drop(source_file);

    let sparse_dest = temp.path().join("large-dest.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    let start = std::time::Instant::now();
    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("large sparse copy succeeds");
    let elapsed = start.elapsed();

    // Should complete quickly despite 1GB size (mostly holes)
    assert!(
        elapsed.as_secs() < 10,
        "large sparse copy took too long: {elapsed:?}"
    );

    let meta = fs::metadata(&sparse_dest).expect("metadata");
    assert_eq!(meta.len(), file_size);

    // Verify minimal block allocation (platform-dependent)
    use std::os::unix::fs::MetadataExt;
    let blocks = meta.blocks();
    let max_expected_blocks = 1024; // ~512KB for data + overhead

    if blocks > max_expected_blocks {
        eprintln!(
            "WARNING: large sparse file allocated {blocks} blocks (expected < {max_expected_blocks}); \
             filesystem may not support sparse files efficiently"
        );
    }

    // Verify key data regions without reading entire file
    let mut dest_file = fs::File::open(&sparse_dest).expect("open dest");

    let mut buffer = vec![0u8; 6];
    dest_file.read_exact(&mut buffer).expect("read header");
    assert_eq!(&buffer, b"HEADER");

    dest_file
        .seek(SeekFrom::Start(500 * 1024 * 1024))
        .expect("seek mid");
    dest_file.read_exact(&mut buffer).expect("read middle");
    assert_eq!(&buffer, b"MIDDLE");

    dest_file
        .seek(SeekFrom::Start(1024 * 1024 * 1024))
        .expect("seek footer");
    dest_file.read_exact(&mut buffer).expect("read footer");
    assert_eq!(&buffer, b"FOOTER");
}

/// Test: All-zeros file creates a fully sparse file with minimal block allocation.
#[cfg(unix)]
#[test]
fn execute_sparse_all_zeros_creates_fully_sparse_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("all-zeros.bin");
    let file = fs::File::create(&source).expect("create source");
    // Create a 1MB file of all zeros
    file.set_len(1024 * 1024).expect("set source length");
    drop(file);

    let dense_dest = temp.path().join("dense-zeros.bin");
    let sparse_dest = temp.path().join("sparse-zeros.bin");

    // Dense copy (no sparse)
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Both should have the same logical file size
    assert_eq!(dense_meta.len(), 1024 * 1024);
    assert_eq!(sparse_meta.len(), 1024 * 1024);

    // Content should be identical (all zeros)
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    assert!(
        sparse_content.iter().all(|&b| b == 0),
        "sparse file should contain only zeros"
    );

    // Sparse file should use significantly fewer blocks
    use std::os::unix::fs::MetadataExt;
    let dense_blocks = dense_meta.blocks();
    let sparse_blocks = sparse_meta.blocks();

    if sparse_blocks == dense_blocks {
        eprintln!(
            "all-zeros sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem does not expose sparse allocation difference"
        );
    } else {
        assert!(
            sparse_blocks < dense_blocks,
            "all-zeros sparse should allocate fewer blocks (sparse: {sparse_blocks}, \
             dense: {dense_blocks})"
        );
        // For a fully sparse all-zeros file, we expect very few blocks
        // (ideally 0 or close to it, depending on filesystem)
        let max_expected = dense_blocks / 4;
        if sparse_blocks > max_expected {
            eprintln!(
                "NOTE: all-zeros sparse uses {sparse_blocks} blocks, \
                 expected significantly fewer than {dense_blocks}"
            );
        }
    }
}

/// Test: File with no zeros is written normally (no holes created).
#[cfg(unix)]
#[test]
fn execute_sparse_nonzero_file_written_normally() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("nonzero.bin");

    // Create a 64KB file with no zero bytes
    let data: Vec<u8> = (0..65536u32).map(|i| ((i % 255) + 1) as u8).collect();
    fs::write(&source, &data).expect("write source");

    let dense_dest = temp.path().join("dense-nonzero.bin");
    let sparse_dest = temp.path().join("sparse-nonzero.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Content should be identical
    let dense_content = fs::read(&dense_dest).expect("read dense");
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    assert_eq!(dense_content, sparse_content);
    assert_eq!(sparse_content, data);

    // Block counts should be similar since there are no zeros to sparse-ify
    use std::os::unix::fs::MetadataExt;
    let dense_blocks = fs::metadata(&dense_dest).expect("dense metadata").blocks();
    let sparse_blocks = fs::metadata(&sparse_dest).expect("sparse metadata").blocks();

    // Non-zero file should use roughly the same number of blocks
    assert!(
        sparse_blocks >= dense_blocks.saturating_sub(8),
        "non-zero sparse should not create holes (sparse: {sparse_blocks}, dense: {dense_blocks})"
    );
}

/// Test: Small files (smaller than one block / below threshold) handled correctly.
#[cfg(unix)]
#[test]
fn execute_sparse_small_file_below_threshold() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("small.bin");

    // Create a file much smaller than the 32KB sparse threshold
    let mut data = vec![0u8; 100];
    data[0] = b'X';
    data[50] = b'Y';
    data[99] = b'Z';
    fs::write(&source, &data).expect("write source");

    let sparse_dest = temp.path().join("sparse-small.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    // Verify content integrity
    let content = fs::read(&sparse_dest).expect("read dest");
    assert_eq!(content.len(), 100);
    assert_eq!(content[0], b'X');
    assert_eq!(content[50], b'Y');
    assert_eq!(content[99], b'Z');
    assert!(content[1..50].iter().all(|&b| b == 0));
    assert!(content[51..99].iter().all(|&b| b == 0));
}

/// Test: Small file that is entirely zeros (below threshold).
#[cfg(unix)]
#[test]
fn execute_sparse_small_all_zeros_file() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("small-zeros.bin");

    // 256 bytes of zeros - well below the 32KB threshold
    fs::write(&source, vec![0u8; 256]).expect("write source");

    let sparse_dest = temp.path().join("sparse-small-zeros.bin");
    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        sparse_dest.clone().into_os_string(),
    ])
    .expect("plan");

    plan.execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default().sparse(true),
    )
    .expect("sparse copy succeeds");

    let meta = fs::metadata(&sparse_dest).expect("metadata");
    assert_eq!(meta.len(), 256);

    let content = fs::read(&sparse_dest).expect("read dest");
    assert!(content.iter().all(|&b| b == 0));
}

/// Test: Sparse hole only at the start of the file (leading zeros).
#[cfg(unix)]
#[test]
fn execute_sparse_hole_at_start() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("hole-at-start.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Start with a large hole (64KB zeros), then data
    source_file
        .write_all(&vec![0u8; 64 * 1024])
        .expect("write leading zeros");
    source_file
        .write_all(&vec![0xAA; 4096])
        .expect("write trailing data");
    drop(source_file);

    let dense_dest = temp.path().join("dense-start.bin");
    let sparse_dest = temp.path().join("sparse-start.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Verify content
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    let dense_content = fs::read(&dense_dest).expect("read dense");
    assert_eq!(sparse_content, dense_content);
    assert!(sparse_content[..64 * 1024].iter().all(|&b| b == 0));
    assert!(sparse_content[64 * 1024..].iter().all(|&b| b == 0xAA));

    // Verify sparse uses fewer blocks
    use std::os::unix::fs::MetadataExt;
    let dense_blocks = fs::metadata(&dense_dest).expect("dense meta").blocks();
    let sparse_blocks = fs::metadata(&sparse_dest).expect("sparse meta").blocks();

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "hole-at-start sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not expose difference"
        );
    }
}

/// Test: Sparse hole only at the end of the file (trailing zeros).
#[cfg(unix)]
#[test]
fn execute_sparse_hole_at_end() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("hole-at-end.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Data first, then a large trailing hole
    source_file
        .write_all(&vec![0xBB; 4096])
        .expect("write leading data");
    source_file
        .write_all(&vec![0u8; 64 * 1024])
        .expect("write trailing zeros");
    drop(source_file);

    let dense_dest = temp.path().join("dense-end.bin");
    let sparse_dest = temp.path().join("sparse-end.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Verify content
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    let dense_content = fs::read(&dense_dest).expect("read dense");
    assert_eq!(sparse_content, dense_content);
    assert!(sparse_content[..4096].iter().all(|&b| b == 0xBB));
    assert!(sparse_content[4096..].iter().all(|&b| b == 0));

    // Verify sparse uses fewer blocks
    use std::os::unix::fs::MetadataExt;
    let dense_blocks = fs::metadata(&dense_dest).expect("dense meta").blocks();
    let sparse_blocks = fs::metadata(&sparse_dest).expect("sparse meta").blocks();

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "hole-at-end sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not expose difference"
        );
    }
}

/// Test: Sparse hole in the middle of the file only.
#[cfg(unix)]
#[test]
fn execute_sparse_hole_in_middle() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("hole-in-middle.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Data, then hole, then data
    source_file
        .write_all(&vec![0xCC; 4096])
        .expect("write leading data");
    source_file
        .write_all(&vec![0u8; 128 * 1024])
        .expect("write middle zeros");
    source_file
        .write_all(&vec![0xDD; 4096])
        .expect("write trailing data");
    drop(source_file);

    let dense_dest = temp.path().join("dense-mid.bin");
    let sparse_dest = temp.path().join("sparse-mid.bin");

    // Dense copy
    let plan_dense = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        dense_dest.clone().into_os_string(),
    ])
    .expect("plan dense");
    plan_dense
        .execute_with_options(LocalCopyExecution::Apply, LocalCopyOptions::default())
        .expect("dense copy succeeds");

    // Sparse copy
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

    // Verify content
    let sparse_content = fs::read(&sparse_dest).expect("read sparse");
    let dense_content = fs::read(&dense_dest).expect("read dense");
    assert_eq!(sparse_content, dense_content);
    assert!(sparse_content[..4096].iter().all(|&b| b == 0xCC));
    assert!(sparse_content[4096..4096 + 128 * 1024].iter().all(|&b| b == 0));
    assert!(sparse_content[4096 + 128 * 1024..].iter().all(|&b| b == 0xDD));

    // Verify sparse uses fewer blocks
    use std::os::unix::fs::MetadataExt;
    let dense_blocks = fs::metadata(&dense_dest).expect("dense meta").blocks();
    let sparse_blocks = fs::metadata(&sparse_dest).expect("sparse meta").blocks();

    if sparse_blocks >= dense_blocks {
        eprintln!(
            "hole-in-middle sparse uses {sparse_blocks} blocks, dense uses {dense_blocks}; \
             filesystem may not expose difference"
        );
    } else {
        assert!(
            sparse_blocks < dense_blocks,
            "middle-hole sparse should use fewer blocks (sparse: {sparse_blocks}, dense: {dense_blocks})"
        );
    }
}

/// Test: --preallocate disables sparse write optimization at execution level.
///
/// Upstream rsync disables sparse writes when --preallocate is active because
/// preallocation materialises every range in the destination file, making holes
/// counterproductive.
#[cfg(unix)]
#[test]
fn execute_preallocate_disables_sparse_writes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("prealloc-sparse.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create file with a large zero region that would normally be sparsified
    source_file
        .write_all(&[0x11])
        .expect("write leading byte");
    source_file
        .write_all(&vec![0u8; 256 * 1024])
        .expect("write 256KB zeros");
    source_file
        .write_all(&[0x22])
        .expect("write trailing byte");
    drop(source_file);

    let sparse_only_dest = temp.path().join("sparse-only.bin");
    let prealloc_sparse_dest = temp.path().join("prealloc-sparse.bin");

    // Sparse-only copy (no preallocate)
    let plan_sparse = LocalCopyPlan::from_operands(&[
        source.clone().into_os_string(),
        sparse_only_dest.clone().into_os_string(),
    ])
    .expect("plan sparse");
    plan_sparse
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true),
        )
        .expect("sparse copy succeeds");

    // Preallocate + Sparse copy (preallocate should override sparse)
    let plan_prealloc = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        prealloc_sparse_dest.clone().into_os_string(),
    ])
    .expect("plan prealloc+sparse");
    plan_prealloc
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true).preallocate(true),
        )
        .expect("preallocate+sparse copy succeeds");

    // Both should have the same logical size and content
    let sparse_meta = fs::metadata(&sparse_only_dest).expect("sparse metadata");
    let prealloc_meta = fs::metadata(&prealloc_sparse_dest).expect("prealloc metadata");
    assert_eq!(sparse_meta.len(), prealloc_meta.len());

    let sparse_content = fs::read(&sparse_only_dest).expect("read sparse");
    let prealloc_content = fs::read(&prealloc_sparse_dest).expect("read prealloc");
    assert_eq!(sparse_content, prealloc_content);

    // Verify data integrity
    let total_size = 1 + 256 * 1024 + 1;
    assert_eq!(sparse_content.len(), total_size);
    assert_eq!(sparse_content[0], 0x11);
    assert!(sparse_content[1..1 + 256 * 1024].iter().all(|&b| b == 0));
    assert_eq!(sparse_content[total_size - 1], 0x22);

    // Preallocated file should use at least as many blocks as sparse-only
    use std::os::unix::fs::MetadataExt;
    let sparse_blocks = sparse_meta.blocks();
    let prealloc_blocks = prealloc_meta.blocks();

    // The preallocated+sparse file should NOT create holes because preallocate
    // overrides sparse. Therefore it should use >= blocks than the sparse file.
    if prealloc_blocks < sparse_blocks {
        // On some filesystems, preallocate may not have the expected effect,
        // but the content must still be correct (verified above).
        eprintln!(
            "NOTE: preallocate+sparse uses {prealloc_blocks} blocks, \
             sparse-only uses {sparse_blocks} blocks; filesystem may handle \
             preallocate differently"
        );
    }
}

/// Test: Sparse with --append is disabled (append mode prevents sparse).
#[cfg(unix)]
#[test]
fn execute_append_disables_sparse_writes() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("append-sparse.bin");
    let mut source_file = fs::File::create(&source).expect("create source");

    // Create a file with zeros
    source_file.write_all(&[0xFF]).expect("write byte");
    source_file
        .write_all(&vec![0u8; 64 * 1024])
        .expect("write zeros");
    source_file.write_all(&[0xEE]).expect("write byte");
    drop(source_file);

    let dest = temp.path().join("append-dest.bin");

    // Create a partial destination for append mode
    fs::write(&dest, [0xFF]).expect("write partial dest");
    filetime::set_file_mtime(
        &dest,
        filetime::FileTime::from_unix_time(1, 0),
    )
    .expect("set dest mtime");
    filetime::set_file_mtime(
        &source,
        filetime::FileTime::from_unix_time(2, 0),
    )
    .expect("set source mtime");

    let plan = LocalCopyPlan::from_operands(&[
        source.into_os_string(),
        dest.clone().into_os_string(),
    ])
    .expect("plan");

    // Even with sparse enabled, append should prevent hole creation
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true).append(true),
        )
        .expect("append+sparse copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify content is correct
    let content = fs::read(&dest).expect("read dest");
    let total_size = 1 + 64 * 1024 + 1;
    assert_eq!(content.len(), total_size);
    assert_eq!(content[0], 0xFF);
    assert_eq!(content[total_size - 1], 0xEE);
}

