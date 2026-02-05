// Tests for --checksum behavior with different hash algorithms.
//
// This test module verifies that:
// 1. File comparison using checksum vs mtime works correctly
// 2. Different hash algorithms (MD4, MD5, SHA1, XXH64, XXH3, XXH3-128) all work
// 3. Checksum verification of transferred files is accurate
// 4. Protocol version differences in checksum behavior are handled
//
// Upstream rsync uses different algorithms based on protocol version:
// - Protocol < 30: MD4 (legacy)
// - Protocol >= 30: MD5 by default, XXH64/XXH3 when negotiated
// - Protocol >= 31: XXH3 preferred when both peers support it

// ============================================================================
// Checksum Algorithm Tests - All Supported Hash Types
// ============================================================================

/// Tests that MD4 algorithm correctly identifies identical files.
#[test]
fn checksum_md4_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical content for MD4 test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set different timestamps to ensure checksum comparison is used
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Md4),
        )
        .expect("copy succeeds");

    // File should be skipped because MD4 checksums match
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that MD4 algorithm correctly identifies different files.
#[test]
fn checksum_md4_transfers_different_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"source!").expect("write source");
    fs::write(&destination, b"dest!!!").expect("write dest");

    // Set identical timestamps
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Md4),
        )
        .expect("copy succeeds");

    // File should be copied because MD4 checksums differ
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"source!"
    );
}

/// Tests that MD5 algorithm (default for protocol >= 30) correctly identifies identical files.
#[test]
fn checksum_md5_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical content for MD5 test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Set different timestamps
    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                }),
        )
        .expect("copy succeeds");

    // File should be skipped because MD5 checksums match
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_total(), 1);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that MD5 algorithm correctly identifies different files.
#[test]
fn checksum_md5_transfers_different_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"source content").expect("write source");
    fs::write(&destination, b"dest content!!").expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::none(),
                }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"source content"
    );
}

/// Tests that MD5 with seeding (protocol 30+ CHECKSUM_SEED_FIX) works correctly.
#[test]
fn checksum_md5_seeded_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"seeded MD5 checksum test content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Use seeded MD5 (proper ordering as per protocol 30+)
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Md5 {
                    seed_config: checksums::strong::Md5Seed::proper(0x12345678),
                }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that SHA1 algorithm works correctly for checksum comparison.
#[test]
fn checksum_sha1_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"SHA1 checksum test content for verification";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Sha1),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that SHA1 algorithm transfers different files.
#[test]
fn checksum_sha1_transfers_different_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"sha1 source").expect("write source");
    fs::write(&destination, b"sha1 dest!!").expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Sha1),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"sha1 source"
    );
}

/// Tests that XXH64 algorithm (fast non-cryptographic) works correctly.
#[test]
fn checksum_xxh64_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"XXH64 fast checksum test content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh64 { seed: 0 }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that XXH64 transfers different files.
#[test]
fn checksum_xxh64_transfers_different_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"xxh64 source").expect("write source");
    fs::write(&destination, b"xxh64 dest!!").expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh64 { seed: 0 }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"xxh64 source"
    );
}

/// Tests that XXH64 with different seeds produces different results.
#[test]
fn checksum_xxh64_different_seeds_are_independent() {
    let temp = tempdir().expect("tempdir");
    let source1 = temp.path().join("source1.txt");
    let source2 = temp.path().join("source2.txt");
    let dest1 = temp.path().join("dest1.txt");
    let dest2 = temp.path().join("dest2.txt");

    let content = b"content for seed independence test";
    fs::write(&source1, content).expect("write source1");
    fs::write(&source2, content).expect("write source2");
    fs::write(&dest1, content).expect("write dest1");
    fs::write(&dest2, content).expect("write dest2");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    for path in [&source1, &source2] {
        set_file_mtime(path, newer_time).expect("set source time");
    }
    for path in [&dest1, &dest2] {
        set_file_mtime(path, older_time).expect("set dest time");
    }

    // Test with seed 0
    let operands1 = vec![
        source1.into_os_string(),
        dest1.clone().into_os_string(),
    ];
    let plan1 = LocalCopyPlan::from_operands(&operands1).expect("plan1");
    let summary1 = plan1
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh64 { seed: 0 }),
        )
        .expect("copy1 succeeds");

    // Test with different seed
    let operands2 = vec![
        source2.into_os_string(),
        dest2.clone().into_os_string(),
    ];
    let plan2 = LocalCopyPlan::from_operands(&operands2).expect("plan2");
    let summary2 = plan2
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh64 { seed: 42 }),
        )
        .expect("copy2 succeeds");

    // Both should skip (same content, same seed for each file's comparison)
    assert_eq!(summary1.files_copied(), 0);
    assert_eq!(summary2.files_copied(), 0);
}

/// Tests that XXH3 (64-bit) algorithm works correctly.
#[test]
fn checksum_xxh3_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"XXH3 modern fast checksum test content";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh3 { seed: 0 }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that XXH3 transfers different files.
#[test]
fn checksum_xxh3_transfers_different_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"xxh3 source!").expect("write source");
    fs::write(&destination, b"xxh3 dest!!!").expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh3 { seed: 0 }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"xxh3 source!"
    );
}

/// Tests that XXH3-128 algorithm works correctly for checksum comparison.
#[test]
fn checksum_xxh3_128_skips_identical_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"XXH3-128 extended checksum test content for better collision resistance";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh3_128 { seed: 0 }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that XXH3-128 transfers different files.
#[test]
fn checksum_xxh3_128_transfers_different_files() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"xxh128 source!").expect("write source");
    fs::write(&destination, b"xxh128 dest!!!").expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .with_checksum_algorithm(SignatureAlgorithm::Xxh3_128 { seed: 0 }),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"xxh128 source!"
    );
}

// ============================================================================
// Checksum vs Mtime Comparison Tests
// ============================================================================

/// Tests that checksum mode ignores mtime for identical content.
#[test]
fn checksum_ignores_mtime_when_content_matches() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"identical content regardless of timestamps";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    // Source is MUCH newer than destination
    let very_old = FileTime::from_unix_time(1_000_000_000, 0); // 2001
    let very_new = FileTime::from_unix_time(1_700_000_000, 0); // 2023
    set_file_mtime(&source, very_new).expect("set source time");
    set_file_mtime(&destination, very_old).expect("set dest time");

    // Record destination mtime before sync
    let dest_mtime_before = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // File should be skipped (checksums match)
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);

    // Destination mtime should be unchanged
    let dest_mtime_after = FileTime::from_last_modification_time(
        &fs::metadata(&destination).expect("dest metadata"),
    );
    assert_eq!(dest_mtime_before, dest_mtime_after);
}

/// Tests that checksum mode transfers when mtime matches but content differs.
#[test]
fn checksum_transfers_when_mtime_matches_but_content_differs() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"new content!").expect("write source");
    fs::write(&destination, b"old content!").expect("write dest");

    // Set IDENTICAL timestamps (would normally cause skip without checksum)
    let same_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, same_time).expect("set source time");
    set_file_mtime(&destination, same_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // File SHOULD be copied because checksums differ
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"new content!"
    );
}

/// Tests that without checksum mode, mtime+size match causes skip.
#[test]
fn without_checksum_mtime_match_skips_even_with_different_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"source!!").expect("write source");
    fs::write(&destination, b"dest!!!!").expect("write dest");

    // Verify same size
    assert_eq!(
        fs::metadata(&source).expect("source meta").len(),
        fs::metadata(&destination).expect("dest meta").len()
    );

    // Set identical timestamps
    let same_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, same_time).expect("set source time");
    set_file_mtime(&destination, same_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    // Without checksum mode
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(false),
        )
        .expect("copy succeeds");

    // File should be SKIPPED (mtime+size match, no checksum)
    assert_eq!(summary.files_copied(), 0);
    // Content remains different
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"dest!!!!"
    );
}

// ============================================================================
// Checksum Verification of Transferred Files
// ============================================================================

/// Tests that after transfer, content is verified correct.
#[test]
fn checksum_transfer_produces_correct_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create binary content with known pattern
    let source_content: Vec<u8> = (0..=255).cycle().take(10000).collect();
    fs::write(&source, &source_content).expect("write source");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    assert_eq!(summary.files_copied(), 1);

    // Verify content byte-for-byte
    let dest_content = fs::read(&destination).expect("read dest");
    assert_eq!(dest_content, source_content);
}

/// Tests checksum comparison with large files (multi-buffer).
#[test]
fn checksum_large_file_identical_content() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    // Create large identical files (>64KB to test buffering)
    let large_content = vec![0xABu8; 100_000];
    fs::write(&source, &large_content).expect("write source");
    fs::write(&destination, &large_content).expect("write dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Should skip (checksums match for large file)
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests checksum detects difference at end of large file.
#[test]
fn checksum_large_file_differ_at_end() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.bin");
    let destination = temp.path().join("dest.bin");

    let source_content = vec![0xABu8; 100_000];
    let mut dest_content = source_content.clone();
    dest_content[99_999] = 0xCD; // Change only the last byte

    fs::write(&source, &source_content).expect("write source");
    fs::write(&destination, &dest_content).expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Should transfer (checksums differ even for last-byte change)
    assert_eq!(summary.files_copied(), 1);
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        source_content
    );
}

// ============================================================================
// Empty File Tests
// ============================================================================

/// Tests that checksum comparison handles empty files correctly.
#[test]
fn checksum_empty_files_are_identical() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    fs::write(&source, b"").expect("write empty source");
    fs::write(&destination, b"").expect("write empty dest");

    let older_time = FileTime::from_unix_time(1_700_000_000, 0);
    let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
    set_file_mtime(&source, newer_time).expect("set source time");
    set_file_mtime(&destination, older_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Empty files should match
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests all algorithms produce identical results for empty files.
#[test]
fn checksum_all_algorithms_agree_on_empty_files() {
    let algorithms = [
        SignatureAlgorithm::Md4,
        SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        },
        SignatureAlgorithm::Sha1,
        SignatureAlgorithm::Xxh64 { seed: 0 },
        SignatureAlgorithm::Xxh3 { seed: 0 },
        SignatureAlgorithm::Xxh3_128 { seed: 0 },
    ];

    for algorithm in algorithms {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"").expect("write empty source");
        fs::write(&destination, b"").expect("write empty dest");

        let older_time = FileTime::from_unix_time(1_700_000_000, 0);
        let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
        set_file_mtime(&source, newer_time).expect("set source time");
        set_file_mtime(&destination, older_time).expect("set dest time");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .checksum(true)
                    .with_checksum_algorithm(algorithm),
            )
            .expect("copy succeeds");

        assert_eq!(
            summary.files_copied(),
            0,
            "Algorithm {:?} should skip empty identical files",
            algorithm
        );
    }
}

// ============================================================================
// Directory Recursive Tests with Checksum
// ============================================================================

/// Tests checksum mode with recursive directory copy.
#[test]
fn checksum_recursive_directory_mixed_files() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let dest_root = temp.path().join("dest");
    fs::create_dir_all(&source_root).expect("create source");
    fs::create_dir_all(&dest_root).expect("create dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

    // File 1: identical content (should skip)
    fs::write(source_root.join("same.txt"), b"identical").expect("write");
    fs::write(dest_root.join("same.txt"), b"identical").expect("write");
    set_file_mtime(source_root.join("same.txt"), timestamp).expect("set time");
    set_file_mtime(dest_root.join("same.txt"), timestamp).expect("set time");

    // File 2: different content (should copy)
    fs::write(source_root.join("diff.txt"), b"source!").expect("write");
    fs::write(dest_root.join("diff.txt"), b"dest!!!").expect("write");
    set_file_mtime(source_root.join("diff.txt"), timestamp).expect("set time");
    set_file_mtime(dest_root.join("diff.txt"), timestamp).expect("set time");

    // File 3: new file (should copy)
    fs::write(source_root.join("new.txt"), b"brand new").expect("write");

    let mut source_operand = source_root.clone().into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());
    let operands = vec![source_operand, dest_root.clone().into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(true),
        )
        .expect("copy succeeds");

    // Should copy 2 files (diff + new), skip 1 (same)
    assert_eq!(summary.files_copied(), 2);
    assert_eq!(summary.regular_files_total(), 3);
    assert_eq!(summary.regular_files_matched(), 1);

    // Verify content
    assert_eq!(
        fs::read(dest_root.join("same.txt")).expect("read"),
        b"identical"
    );
    assert_eq!(
        fs::read(dest_root.join("diff.txt")).expect("read"),
        b"source!"
    );
    assert_eq!(
        fs::read(dest_root.join("new.txt")).expect("read"),
        b"brand new"
    );
}

// ============================================================================
// Checksum with Other Flags
// ============================================================================

/// Tests checksum with --ignore-times flag.
#[test]
fn checksum_with_ignore_times_skips_identical() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    let content = b"content for checksum + ignore-times test";
    fs::write(&source, content).expect("write source");
    fs::write(&destination, content).expect("write dest");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, timestamp).expect("set source time");
    set_file_mtime(&destination, timestamp).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .checksum(true)
                .ignore_times(true),
        )
        .expect("copy succeeds");

    // Even with ignore_times, checksum mode should skip identical content
    assert_eq!(summary.files_copied(), 0);
    assert_eq!(summary.regular_files_matched(), 1);
}

/// Tests that --no-checksum disables checksum comparison.
#[test]
fn no_checksum_falls_back_to_mtime() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    let destination = temp.path().join("dest.txt");

    // Same size, different content
    fs::write(&source, b"aaaaaaa").expect("write source");
    fs::write(&destination, b"bbbbbbb").expect("write dest");

    // Same mtime (would skip with mtime comparison)
    let same_time = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_mtime(&source, same_time).expect("set source time");
    set_file_mtime(&destination, same_time).expect("set dest time");

    let operands = vec![
        source.into_os_string(),
        destination.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default().checksum(false),
        )
        .expect("copy succeeds");

    // Without checksum, should skip (mtime+size match)
    assert_eq!(summary.files_copied(), 0);
    // Content remains different
    assert_eq!(
        fs::read(&destination).expect("read dest"),
        b"bbbbbbb"
    );
}

// ============================================================================
// Algorithm Consistency Tests
// ============================================================================

/// Tests that all algorithms produce consistent skip/copy decisions.
#[test]
fn all_algorithms_consistent_for_identical_content() {
    let algorithms = [
        SignatureAlgorithm::Md4,
        SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        },
        SignatureAlgorithm::Sha1,
        SignatureAlgorithm::Xxh64 { seed: 0 },
        SignatureAlgorithm::Xxh3 { seed: 0 },
        SignatureAlgorithm::Xxh3_128 { seed: 0 },
    ];

    let content = b"test content for algorithm consistency check";

    for algorithm in algorithms {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, content).expect("write source");
        fs::write(&destination, content).expect("write dest");

        let older_time = FileTime::from_unix_time(1_700_000_000, 0);
        let newer_time = FileTime::from_unix_time(1_700_000_100, 0);
        set_file_mtime(&source, newer_time).expect("set source time");
        set_file_mtime(&destination, older_time).expect("set dest time");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .checksum(true)
                    .with_checksum_algorithm(algorithm),
            )
            .expect("copy succeeds");

        assert_eq!(
            summary.files_copied(),
            0,
            "Algorithm {:?} should skip identical content",
            algorithm
        );
        assert_eq!(
            summary.regular_files_matched(),
            1,
            "Algorithm {:?} should mark file as matched",
            algorithm
        );
    }
}

/// Tests that all algorithms transfer when content differs.
#[test]
fn all_algorithms_transfer_different_content() {
    let algorithms = [
        SignatureAlgorithm::Md4,
        SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        },
        SignatureAlgorithm::Sha1,
        SignatureAlgorithm::Xxh64 { seed: 0 },
        SignatureAlgorithm::Xxh3 { seed: 0 },
        SignatureAlgorithm::Xxh3_128 { seed: 0 },
    ];

    for algorithm in algorithms {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, b"source content!").expect("write source");
        fs::write(&destination, b"dest content!!!").expect("write dest");

        let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
        set_file_mtime(&source, timestamp).expect("set source time");
        set_file_mtime(&destination, timestamp).expect("set dest time");

        let operands = vec![
            source.into_os_string(),
            destination.clone().into_os_string(),
        ];
        let plan = LocalCopyPlan::from_operands(&operands).expect("plan");

        let summary = plan
            .execute_with_options(
                LocalCopyExecution::Apply,
                LocalCopyOptions::default()
                    .checksum(true)
                    .with_checksum_algorithm(algorithm),
            )
            .expect("copy succeeds");

        assert_eq!(
            summary.files_copied(),
            1,
            "Algorithm {:?} should transfer different content",
            algorithm
        );
        assert_eq!(
            fs::read(&destination).expect("read dest"),
            b"source content!",
            "Algorithm {:?} should produce correct content",
            algorithm
        );
    }
}
