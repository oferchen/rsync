#[test]
fn execute_delta_copy_reuses_existing_blocks() {
    let temp = tempdir().expect("tempdir");
    let source_root = temp.path().join("source");
    let target_root = temp.path().join("target");
    fs::create_dir_all(&source_root).expect("create source root");
    fs::create_dir_all(&target_root).expect("create target root");

    let source_path = source_root.join("file.bin");
    let dest_path = target_root.join("file.bin");

    let mut prefix = vec![b'A'; 700];
    let mut suffix = vec![b'B'; 700];
    let mut replacement = vec![b'C'; 700];
    let prefix_len = prefix.len() as u64;
    let replacement_len = replacement.len() as u64;

    let mut initial = Vec::new();
    initial.append(&mut prefix.clone());
    initial.append(&mut suffix);
    fs::write(&dest_path, &initial).expect("write initial destination");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("set destination mtime");

    let mut updated = Vec::new();
    updated.append(&mut prefix);
    updated.append(&mut replacement);
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
            LocalCopyOptions::default().whole_file(false),
        )
        .expect("delta copy succeeds");

    assert_eq!(summary.files_copied(), 1);
    assert_eq!(summary.bytes_copied(), replacement_len);
    assert_eq!(summary.matched_bytes(), prefix_len);
    assert_eq!(fs::read(&dest_path).expect("read destination"), updated);
}

#[test]
fn execute_with_report_dry_run_records_file_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("source.txt");
    fs::write(&source, b"dry-run").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![source.into_os_string(), destination.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record.action(), &LocalCopyAction::DataCopied);
    assert_eq!(record.relative_path(), Path::new("source.txt"));
    assert_eq!(record.bytes_transferred(), 7);
}

#[test]
fn execute_with_report_dry_run_records_directory_event() {
    use std::fs;

    let temp = tempdir().expect("tempdir");
    let source_dir = temp.path().join("tree");
    fs::create_dir(&source_dir).expect("create source dir");
    fs::write(source_dir.join("file.txt"), b"data").expect("write nested file");
    let destination = temp.path().join("target");

    let operands = vec![source_dir.into_os_string(), destination.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default().collect_events(true);
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    let records = report.records();
    assert!(records.iter().any(|record| {
        record.action() == &LocalCopyAction::DirectoryCreated
            && record.relative_path() == Path::new("tree")
    }));
}

#[test]
fn execute_with_report_records_min_size_skip_notice_without_copying() {
    let temp = tempdir().expect("tempdir");
    let source = temp.path().join("tiny.txt");
    fs::write(&source, b"abc").expect("write source");
    let destination = temp.path().join("dest.txt");

    let operands = vec![source.into_os_string(), destination.into_os_string()];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let options = LocalCopyOptions::default()
        .collect_events(true)
        .min_file_size(Some(10));
    let report = plan
        .execute_with_report(LocalCopyExecution::DryRun, options)
        .expect("dry run succeeds");

    // A `--min-size` skip records a `SkippedUnderMinSize` notice action, exactly
    // like the other generator-phase skips (`SkippedNewerDestination`,
    // `SkippedExisting`, `SkippedMissingDestination`) upstream emits during the
    // generator loop. The record is the only channel that carries the
    // "%s is under min-size" notice to the renderer, which prints it ahead of
    // the statistics block (upstream: generator.c:1712-1719; ClientEvents derive
    // from records() in core `ClientSummary::from_report`). It is a skip notice,
    // not a copy: no data is written and no file is counted as copied.
    let records = report.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].action(), &LocalCopyAction::SkippedUnderMinSize);
    assert_eq!(report.summary().files_copied(), 0);
    assert_eq!(report.summary().regular_files_total(), 1);
    assert_eq!(report.summary().bytes_copied(), 0);
}

/// Deterministic pseudo-random bytes from a 64-bit LCG (Knuth's MMIX
/// constants), taking the high bits so consecutive outputs do not share low-bit
/// structure.
///
/// The generator matters as much as the sizes here. A short-period or repeating
/// fixture lets an unaligned window match some earlier full block purely by
/// content, which swallows the tail and makes the test assert about the data
/// rather than about the matcher. This period is far longer than any fixture
/// below.
fn lcg_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..len)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 33) as u8
        })
        .collect()
}

/// Runs one local delta copy whose basis is `full_blocks` whole blocks plus
/// `tail` trailing bytes, with the source's LAST FULL block replaced.
///
/// Returns `(matched_bytes, literal_bytes)`.
///
/// Modifying the last full block is what makes the fixture load-bearing. It
/// stops the scan matching block-aligned right up to the end, so the loop
/// slides byte-by-byte and reaches EOF holding `block - 1` bytes - a full
/// window. An identical source instead has a match clear the window exactly
/// `tail` bytes early, which is the reachability accident that let the tail
/// probe look correct: such a fixture passes with or without the fix.
fn delta_tail_fixture(block: usize, full_blocks: usize, tail: usize) -> (u64, u64, bool) {
    let temp = tempdir().expect("tempdir");
    let source_path = temp.path().join("source.bin");
    let dest_path = temp.path().join("dest.bin");

    let len = full_blocks * block + tail;
    let basis = lcg_bytes(len, 0x5DEE_CE66_D1CE_4B9D);
    let mut source = basis.clone();
    let modified_start = (full_blocks - 1) * block;
    source[modified_start..modified_start + block]
        .copy_from_slice(&lcg_bytes(block, 0x0BAD_C0DE_0BAD_C0DE));

    fs::write(&dest_path, &basis).expect("write basis");
    set_file_mtime(&dest_path, FileTime::from_unix_time(1, 0)).expect("dest mtime");
    fs::write(&source_path, &source).expect("write source");
    set_file_mtime(&source_path, FileTime::from_unix_time(2, 0)).expect("source mtime");

    let operands = vec![
        source_path.into_os_string(),
        dest_path.clone().into_os_string(),
    ];
    let plan = LocalCopyPlan::from_operands(&operands).expect("plan");
    let block_size = NonZeroU32::new(block as u32).expect("block size is non-zero");
    let summary = plan
        .execute_with_options(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default()
                .whole_file(false)
                .with_block_size_override(Some(block_size)),
        )
        .expect("delta copy succeeds");

    let exact = fs::read(&dest_path).expect("read dest") == source;
    (summary.matched_bytes(), summary.bytes_copied(), exact)
}

#[test]
fn delta_matches_the_basis_short_final_block_at_a_full_eof_window() {
    // 292 x 700 + 400, the shape upstream was measured on. The 400-byte
    // remainder is the only basis block whose recorded length is below the
    // block length, so it is the only one that can satisfy upstream's
    // `l = MIN(blength, len-offset); if (l != s->sums[i].len) continue;`
    // (match.c:222-224) - and only at the file's own end, because upstream's
    // scan is bounded by that block's length
    // (`end = len + 1 - s->sums[s->count-1].len`, match.c:174) and its window
    // shrinks to reach it (match.c:321,331).
    //
    // WHY THIS ASSERTION AND NOT BYTE-EQUALITY: reconstruction is byte-exact
    // either way. The defect is that the trailing block is re-sent as literal
    // data that upstream matches, so only the literal/matched split can see it.
    let (matched, literal, exact) = delta_tail_fixture(700, 292, 400);

    assert!(exact, "reconstruction must be byte-exact");
    // 291 untouched full blocks (203,700) plus the 400-byte tail.
    assert_eq!(
        matched, 204_100,
        "the basis's short final block must be matched, not re-sent"
    );
    // Only the one modified full block is literal. Before the fix this was
    // 1,100: the 700-byte modified block plus the 400-byte tail.
    assert_eq!(literal, 700, "only the modified block may be literal");
}

#[test]
fn delta_tail_match_handles_a_single_byte_final_block() {
    // tail_len == 1 is the narrowest short block that exists. zsync 0.6 fixed
    // an out-of-bounds access "when processing the last block of a
    // non-compressed download", so the degenerate widths are exactly where this
    // family of implementations has gone wrong before.
    let (matched, literal, exact) = delta_tail_fixture(700, 4, 1);

    assert!(exact, "reconstruction must be byte-exact");
    assert_eq!(matched, 3 * 700 + 1, "a one-byte final block still matches");
    assert_eq!(literal, 700);
}

#[test]
fn delta_tail_match_handles_a_final_block_one_byte_short() {
    // tail_len == block_length - 1 is the widest short block. It is the width
    // the EOF window itself happens to hold, so a probe that used "whatever is
    // left in the window" would pass here while failing every other width -
    // this pins that the probe length comes from the basis, not from the
    // window.
    let (matched, literal, exact) = delta_tail_fixture(700, 4, 699);

    assert!(exact, "reconstruction must be byte-exact");
    assert_eq!(matched, 3 * 700 + 699);
    assert_eq!(literal, 700);
}

#[test]
fn delta_tail_probe_is_a_no_op_when_the_basis_has_no_short_block() {
    // An exact multiple has no final short block, so upstream has nothing extra
    // to offer and the probe must not fire at all. If it degraded to matching
    // "whatever is left in the window" it could emit a spurious short Copy here.
    let (matched, literal, exact) = delta_tail_fixture(700, 4, 0);

    assert!(exact, "reconstruction must be byte-exact");
    assert_eq!(matched, 3 * 700, "no tail block exists to match");
    assert_eq!(literal, 700, "only the modified block is literal");
}
