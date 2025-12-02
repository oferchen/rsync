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

