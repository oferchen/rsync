// Touched-block accounting for the protocol-33 `--stats` line
// `Number of 4 KiB logical blocks touched`.
//
// Each case reproduces a scenario from upstream rsync 3.5.1's
// `testsuite/write-touched-blocks_test.py` and asserts the exact count that
// test expects, so a local copy prints the same figure upstream's forked
// receiver reports. upstream: fileio.c:212-243, receiver.c:489, main.c:446-448.

/// Deterministic, incompressible 4 MiB payload (xorshift64), so no block of a
/// "changed" file accidentally matches the basis.
fn touched_blocks_payload(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

const TOUCHED_BLOCKS_FILE_LEN: usize = 4 * 1024 * 1024;

/// Copies `source` over an existing `dest` the way upstream's test drives it:
/// `-a --inplace -I --no-whole-file`.
fn touched_blocks_inplace_delta(source: &Path, dest: &Path) -> LocalCopySummary {
    LocalCopyPlan::from_operands(&[
        source.as_os_str().to_os_string(),
        dest.as_os_str().to_os_string(),
    ])
    .expect("plan")
    .execute_with_options(
        LocalCopyExecution::Apply,
        LocalCopyOptions::default()
            .inplace(true)
            .ignore_times(true)
            .whole_file(false),
    )
    .expect("copy succeeds")
}

/// Writes `data` to a fresh basis/destination pair and returns their paths.
fn touched_blocks_pair(root: &Path, name: &str, data: &[u8]) -> (PathBuf, PathBuf) {
    let source = root.join(format!("{name}.src"));
    let dest = root.join(format!("{name}.dst"));
    fs::write(&source, data).expect("write source");
    fs::write(&dest, data).expect("write dest");
    (source, dest)
}

/// WHY: upstream TEST 1 zeroes the first 3000 bytes; only the first two
/// 2 KiB delta blocks change, both inside 4 KiB block 0, and the unchanged
/// in-place blocks are seeked past - so exactly 1 block is touched.
#[test]
fn touched_blocks_contiguous_change_counts_one() {
    let temp = tempdir().expect("tempdir");
    let data = touched_blocks_payload(1, TOUCHED_BLOCKS_FILE_LEN);
    let (source, dest) = touched_blocks_pair(temp.path(), "contig", &data);
    let mut changed = data;
    changed[..3000].fill(0);
    fs::write(&source, &changed).expect("rewrite source");

    let summary = touched_blocks_inplace_delta(&source, &dest);
    assert_eq!(summary.touched_blocks_4k(), 1);
    assert_eq!(fs::read(&dest).expect("read dest"), changed);
}

/// WHY: upstream TEST 2 flips one byte at the start of each of 4 KiB blocks
/// 1..=10; each literal lands in its own block, so the count is 10.
#[test]
fn touched_blocks_scattered_changes_count_ten() {
    let temp = tempdir().expect("tempdir");
    let data = touched_blocks_payload(2, TOUCHED_BLOCKS_FILE_LEN);
    let (source, dest) = touched_blocks_pair(temp.path(), "scatter", &data);
    let mut changed = data;
    for i in 1..=10 {
        changed[i * 4096] ^= 0xFF;
    }
    fs::write(&source, &changed).expect("rewrite source");

    let summary = touched_blocks_inplace_delta(&source, &dest);
    assert_eq!(summary.touched_blocks_4k(), 10);
    assert_eq!(fs::read(&dest).expect("read dest"), changed);
}

/// WHY: upstream TEST 3 - identical files write nothing in place, so 0.
#[test]
fn touched_blocks_identical_files_count_zero() {
    let temp = tempdir().expect("tempdir");
    let data = touched_blocks_payload(3, TOUCHED_BLOCKS_FILE_LEN);
    let (source, dest) = touched_blocks_pair(temp.path(), "same", &data);

    let summary = touched_blocks_inplace_delta(&source, &dest);
    assert_eq!(summary.touched_blocks_4k(), 0);
}

/// WHY: upstream TEST 4 - wholly new content matches nothing, so all 1,024
/// blocks of the 4 MiB file are written.
#[test]
fn touched_blocks_full_rewrite_counts_every_block() {
    let temp = tempdir().expect("tempdir");
    let data = touched_blocks_payload(4, TOUCHED_BLOCKS_FILE_LEN);
    let (source, dest) = touched_blocks_pair(temp.path(), "full", &data);
    fs::write(&source, touched_blocks_payload(44, TOUCHED_BLOCKS_FILE_LEN))
        .expect("rewrite source");

    let summary = touched_blocks_inplace_delta(&source, &dest);
    assert_eq!(summary.touched_blocks_4k(), 1024);
}

/// WHY: the in-place skip is what keeps TEST 2 at 10. Without --inplace the
/// receiver rebuilds a temp file and writes every matched block too, so the
/// same scattered change touches all 1,024 blocks.
#[test]
fn touched_blocks_temp_file_rebuild_counts_matched_blocks() {
    let temp = tempdir().expect("tempdir");
    let data = touched_blocks_payload(5, TOUCHED_BLOCKS_FILE_LEN);
    let (source, dest) = touched_blocks_pair(temp.path(), "rebuild", &data);
    let mut changed = data;
    for i in 1..=10 {
        changed[i * 4096] ^= 0xFF;
    }
    fs::write(&source, &changed).expect("rewrite source");

    let summary =
        LocalCopyPlan::from_operands(&[source.into_os_string(), dest.clone().into_os_string()])
            .expect("plan")
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .ignore_times(true)
                    .whole_file(false),
            )
            .expect("copy succeeds");
    assert_eq!(summary.touched_blocks_4k(), 1024);
    assert_eq!(fs::read(&dest).expect("read dest"), changed);
}

/// WHY: upstream TEST 5 - a fresh `--sparse` copy of `4 KiB data + 4 MiB hole
/// + 4 KiB data` seeks over the hole, so only the 2 data blocks count.
#[cfg(unix)]
#[test]
fn touched_blocks_sparse_hole_is_not_counted() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("sparse.bin");
    let dest = temp.path().join("sparse_dest.bin");
    let mut file = fs::File::create(&source).expect("create source");
    file.write_all(&touched_blocks_payload(6, 4096))
        .expect("write head");
    file.seek(SeekFrom::Current(4 * 1024 * 1024))
        .expect("seek hole");
    file.write_all(&touched_blocks_payload(66, 4096))
        .expect("write tail");
    drop(file);

    let summary = LocalCopyPlan::from_operands(&[source.into_os_string(), dest.into_os_string()])
        .expect("plan")
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().sparse(true),
        )
        .expect("copy succeeds");
    assert_eq!(summary.touched_blocks_4k(), 2);
}

/// WHY: upstream TEST 6 - two 4 KiB files in one run. The tracker's per-file
/// reset keeps the second file's block 0 from hiding under the first file's
/// high-water mark, so the count is 2, not 1.
#[test]
fn touched_blocks_reset_per_file() {
    let temp = tempdir().expect("tempdir");
    let src_dir = temp.path().join("fd_test");
    let dst_dir = temp.path().join("fd_dest");
    fs::create_dir_all(&src_dir).expect("create source dir");
    fs::create_dir_all(&dst_dir).expect("create dest dir");
    fs::write(src_dir.join("fileA.bin"), touched_blocks_payload(7, 4096)).expect("write A");
    fs::write(src_dir.join("fileB.bin"), touched_blocks_payload(77, 4096)).expect("write B");

    let mut source_operand = src_dir.into_os_string();
    source_operand.push("/");
    let mut dest_operand = dst_dir.into_os_string();
    dest_operand.push("/");
    let summary = LocalCopyPlan::from_operands(&[source_operand, dest_operand])
        .expect("plan")
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().inplace(true),
        )
        .expect("copy succeeds");
    assert_eq!(summary.touched_blocks_4k(), 2);
}

/// WHY: an in-place matched block that lands at a different offset is
/// written, not seeked past, so it is a touched block. Rotating the file by one
/// 2 KiB delta block moves every block, so all 1,024 4 KiB blocks are touched
/// even though almost no literal data is sent.
/// upstream: receiver.c:640-649 - only `offset == offset2` takes skip_matched().
#[test]
fn touched_blocks_inplace_relocated_blocks_are_counted() {
    let temp = tempdir().expect("tempdir");
    let data = touched_blocks_payload(8, TOUCHED_BLOCKS_FILE_LEN);
    let (source, dest) = touched_blocks_pair(temp.path(), "rotate", &data);
    let mut rotated = data[2048..].to_vec();
    rotated.extend_from_slice(&data[..2048]);
    fs::write(&source, &rotated).expect("rewrite source");

    let summary = touched_blocks_inplace_delta(&source, &dest);
    assert_eq!(summary.touched_blocks_4k(), 1024);
    assert_eq!(fs::read(&dest).expect("read dest"), rotated);
}
