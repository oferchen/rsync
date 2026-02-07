use super::common::*;
use super::*;

// ============================================================================
// Level 0 (default): minimal output, no file listing
// ============================================================================

/// Verifies that with no verbosity flags, stdout contains no file listing and
/// no summary totals. Upstream rsync produces no output at verbosity 0 for a
/// simple file transfer.
#[test]
fn level_0_no_file_listing() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("quiet.txt");
    let destination = tmp.path().join("quiet.out");
    std::fs::write(&source, b"quiet").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    // Level 0: no output at all.
    assert!(
        stdout.is_empty(),
        "level 0 should produce no stdout, got: {:?}",
        String::from_utf8_lossy(&stdout)
    );
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"quiet"
    );
}

/// Verifies that level 0 produces no summary totals line (sent/received).
#[test]
fn level_0_no_summary_totals() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("nosummary.txt");
    let destination = tmp.path().join("nosummary.out");
    std::fs::write(&source, b"no summary").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        !rendered.contains("sent"),
        "level 0 should not contain 'sent' totals line"
    );
    assert!(
        !rendered.contains("total size is"),
        "level 0 should not contain 'total size is' line"
    );
}

// ============================================================================
// Level 1 (-v): transferred file names shown
// ============================================================================

/// Verifies that -v produces file names and summary totals.
#[test]
fn verbose_transfer_emits_event_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.txt");
    let destination = tmp.path().join("out.txt");
    std::fs::write(&source, b"verbose").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("file.txt"));
    assert!(!rendered.contains("Total transferred"));
    assert!(rendered.contains("sent 7 bytes  received 7 bytes"));
    assert!(rendered.contains("total size is 7  speedup is 0.50"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"verbose"
    );
}

/// Verifies that -v shows the sent/received totals summary line.
#[test]
fn level_1_shows_summary_totals() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("totals.txt");
    let destination = tmp.path().join("totals.out");
    std::fs::write(&source, b"totals test").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("sent"),
        "level 1 should show 'sent ... bytes  received ... bytes' totals"
    );
    assert!(
        rendered.contains("bytes/sec"),
        "level 1 should show rate in totals"
    );
    assert!(
        rendered.contains("total size is"),
        "level 1 should show 'total size is' speedup line"
    );
}

/// Verifies that -v lists multiple files when transferring a directory.
#[test]
fn level_1_lists_multiple_transferred_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("multi_src");
    std::fs::create_dir_all(&source_dir).expect("mkdir");
    std::fs::write(source_dir.join("alpha.txt"), b"aaa").expect("write alpha");
    std::fs::write(source_dir.join("beta.txt"), b"bbb").expect("write beta");

    let dest_dir = tmp.path().join("multi_dst");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("alpha.txt"),
        "level 1 should list alpha.txt, got: {rendered:?}"
    );
    assert!(
        rendered.contains("beta.txt"),
        "level 1 should list beta.txt, got: {rendered:?}"
    );
}

/// Verifies that at level 1, when a file is newly transferred, the file name
/// IS present in the output. This confirms the positive listing behavior.
#[test]
fn level_1_lists_newly_transferred_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("new_transfer.txt");
    let destination = tmp.path().join("new_transfer.out");
    std::fs::write(&source, b"brand new content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("new_transfer.txt"),
        "level 1 should list newly transferred files, got: {rendered:?}"
    );
    assert_eq!(
        std::fs::read(destination).expect("read"),
        b"brand new content"
    );
}

#[cfg(unix)]
#[test]
fn verbose_transfer_reports_skipped_specials() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_fifo = tmp.path().join("skip.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = tmp.path().join("dest.pipe");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source_fifo.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(std::fs::symlink_metadata(&destination).is_err());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("skipping non-regular file \"skip.pipe\""));
}

// ============================================================================
// Level 2 (-vv): more detail, descriptor prefix
// ============================================================================

#[test]
fn verbose_human_readable_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sizes.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_default = tmp.path().join("default.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.clone().into_os_string(),
        dest_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1,536 bytes"));

    let dest_human = tmp.path().join("human.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        dest_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1.54K bytes"));
}

#[test]
fn verbose_human_readable_combined_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sizes.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination = tmp.path().join("combined.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1.54K (1,536) bytes"));
}

/// Verifies that -vv produces a descriptor-prefixed listing (e.g. "data-copied: file").
/// At verbosity >= 2, the render layer uses `describe_event_kind` for each event.
#[test]
fn level_2_shows_descriptor_prefix() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("descriptor.txt");
    let destination = tmp.path().join("descriptor.out");
    std::fs::write(&source, b"descriptor test content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    let rendered = String::from_utf8(stdout).expect("utf8");
    // At verbosity >= 2, the output includes a "descriptor: filename (N bytes, ...)" format.
    assert!(
        rendered.contains("descriptor.txt"),
        "level 2 should list the file name, got: {rendered:?}"
    );
    // The descriptor line should show byte counts.
    assert!(
        rendered.contains("bytes"),
        "level 2 should show byte information, got: {rendered:?}"
    );
}

/// Verifies that -vv still includes the summary totals line.
#[test]
fn level_2_includes_summary_totals() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("vv_totals.txt");
    let destination = tmp.path().join("vv_totals.out");
    std::fs::write(&source, b"vv totals").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("sent"),
        "level 2 should include summary totals, got: {rendered:?}"
    );
    assert!(
        rendered.contains("total size is"),
        "level 2 should include speedup line, got: {rendered:?}"
    );
}

// ============================================================================
// Level 3+ (-vvv): maximum detail
// ============================================================================

/// Verifies that -vvv produces output that is at least as verbose as -vv.
/// It should still contain the descriptor prefix and totals.
#[test]
fn level_3_at_least_as_verbose_as_level_2() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("vvv.txt");
    let destination = tmp.path().join("vvv.out");
    std::fs::write(&source, b"vvv content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vvv"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("vvv.txt"),
        "level 3 should list the file name, got: {rendered:?}"
    );
    assert!(
        rendered.contains("bytes"),
        "level 3 should show byte counts, got: {rendered:?}"
    );
    assert!(
        rendered.contains("sent"),
        "level 3 should show totals, got: {rendered:?}"
    );
}

// ============================================================================
// Verbosity with --dry-run: same file listing behavior
// ============================================================================

/// Verifies that -nv (dry-run + verbose) lists file names on stdout without
/// modifying the destination, matching upstream behavior.
#[test]
fn verbose_with_dry_run_lists_files() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("drysrc");
    std::fs::create_dir_all(&source_dir).expect("mkdir");
    std::fs::write(source_dir.join("a.txt"), b"aaa").expect("write a");
    std::fs::write(source_dir.join("b.txt"), b"bbb").expect("write b");

    let dest_dir = tmp.path().join("drydst");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-nv"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("a.txt"),
        "dry-run -v should list a.txt: {rendered:?}"
    );
    assert!(
        rendered.contains("b.txt"),
        "dry-run -v should list b.txt: {rendered:?}"
    );
    assert!(!dest_dir.exists(), "dry-run must not create destination");
}

/// Verifies that dry-run at level 0 produces no file listing output.
#[test]
fn dry_run_without_verbose_no_file_listing() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("dry_quiet.txt");
    let destination = tmp.path().join("dry_quiet.out");
    std::fs::write(&source, b"dry quiet").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-n"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(
        stdout.is_empty(),
        "dry-run without verbose should produce no stdout, got: {:?}",
        String::from_utf8_lossy(&stdout)
    );
    assert!(!destination.exists());
}

// ============================================================================
// Verbosity with --stats: stats always shown at -v or above
// ============================================================================

/// Verifies that --stats with -v produces both file listings and statistics.
#[test]
fn verbose_with_stats_shows_statistics() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("vstats.txt");
    let destination = tmp.path().join("vstats.out");
    let payload = b"stats verbose content";
    std::fs::write(&source, payload).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--stats"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8");
    // File listing from -v.
    assert!(
        rendered.contains("vstats.txt"),
        "verbose --stats should list the file name, got: {rendered:?}"
    );
    // Stats block.
    assert!(
        rendered.contains("Number of files:"),
        "verbose --stats should contain Number of files, got: {rendered:?}"
    );
    assert!(
        rendered.contains("Total file size:"),
        "verbose --stats should contain Total file size, got: {rendered:?}"
    );
    assert!(
        rendered.contains("Literal data:"),
        "verbose --stats should contain Literal data, got: {rendered:?}"
    );
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

/// Verifies that --stats without any verbosity still shows the stats block.
/// Upstream rsync: --stats implies at minimum the statistics output.
#[test]
fn stats_without_verbose_shows_statistics() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("stats_only.txt");
    let destination = tmp.path().join("stats_only.out");
    std::fs::write(&source, b"stats only").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8");
    // Stats block should be present even at level 0.
    assert!(
        rendered.contains("Number of files:"),
        "--stats should always show statistics block, got: {rendered:?}"
    );
    assert!(
        rendered.contains("Total file size:"),
        "--stats should always show Total file size, got: {rendered:?}"
    );
}

// ============================================================================
// Verbosity with --delete: deletion messages shown
// ============================================================================

/// Verifies that -v --delete shows deletion messages for extraneous files.
#[test]
fn verbose_with_delete_shows_deletion_messages() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("del_src");
    let dest_dir = tmp.path().join("del_dst");
    fs::create_dir_all(&source_dir).expect("mkdir source");
    fs::create_dir_all(&dest_dir).expect("mkdir dest");

    // Source has one file; dest has two (orphan should be deleted).
    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old keep").expect("write dest keep");
    fs::write(dest_dir.join("orphan.txt"), b"orphan").expect("write orphan");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rv"),
        OsString::from("--delete"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let rendered = String::from_utf8(stdout).expect("utf8");
    // Upstream rsync -v --delete shows "deleting <file>" lines.
    assert!(
        rendered.contains("orphan.txt"),
        "verbose --delete should mention the deleted file, got: {rendered:?}"
    );
    // The orphan should actually be deleted.
    assert!(
        !dest_dir.join("orphan.txt").exists(),
        "orphan.txt should have been deleted"
    );
}

/// Verifies that --delete without verbose does not produce deletion messages
/// on stdout (the file is still deleted).
#[test]
fn delete_without_verbose_no_deletion_messages() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("del_quiet_src");
    let dest_dir = tmp.path().join("del_quiet_dst");
    fs::create_dir_all(&source_dir).expect("mkdir source");
    fs::create_dir_all(&dest_dir).expect("mkdir dest");

    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old").expect("write dest keep");
    fs::write(dest_dir.join("stale.txt"), b"stale").expect("write stale");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-r"),
        OsString::from("--delete"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    // At level 0, there should be no output referencing the deletion.
    assert!(
        stdout.is_empty(),
        "delete without verbose should produce no stdout, got: {:?}",
        String::from_utf8_lossy(&stdout)
    );
    // But the file should still be gone.
    assert!(
        !dest_dir.join("stale.txt").exists(),
        "stale.txt should have been deleted"
    );
}

// ============================================================================
// Short flag (-v) and long flag (--verbose) are equivalent
// ============================================================================

/// Verifies that --verbose produces the same output shape as -v.
#[test]
fn long_verbose_flag_equivalent_to_short() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("equiv.txt");
    std::fs::write(&source, b"equivalence test").expect("write source");

    // Run with -v.
    let dest_short = tmp.path().join("short.out");
    let (code_short, stdout_short, stderr_short) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.clone().into_os_string(),
        dest_short.into_os_string(),
    ]);

    // Run with --verbose.
    let dest_long = tmp.path().join("long.out");
    let (code_long, stdout_long, stderr_long) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--verbose"),
        source.into_os_string(),
        dest_long.into_os_string(),
    ]);

    assert_eq!(code_short, 0);
    assert_eq!(code_long, 0);
    assert!(stderr_short.is_empty());
    assert!(stderr_long.is_empty());

    let rendered_short = String::from_utf8(stdout_short).expect("utf8");
    let rendered_long = String::from_utf8(stdout_long).expect("utf8");

    // Both should contain the file name.
    assert!(
        rendered_short.contains("equiv.txt"),
        "-v should show equiv.txt"
    );
    assert!(
        rendered_long.contains("equiv.txt"),
        "--verbose should show equiv.txt"
    );

    // Both should contain the totals.
    assert!(rendered_short.contains("sent"));
    assert!(rendered_long.contains("sent"));
    assert!(rendered_short.contains("total size is"));
    assert!(rendered_long.contains("total size is"));
}

/// Verifies that parse_args treats -v and --verbose identically.
#[test]
fn parse_args_short_v_equals_long_verbose() {
    let short_parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse -v");

    let long_parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--verbose"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse --verbose");

    assert_eq!(short_parsed.verbosity, long_parsed.verbosity);
    assert_eq!(short_parsed.verbosity, 1);
}

// ============================================================================
// Multiple -v flags increase level incrementally
// ============================================================================

/// Verifies that each additional -v increments the verbosity level.
#[test]
fn multiple_v_flags_increment_verbosity() {
    let v1 = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -v");
    assert_eq!(v1.verbosity, 1);

    let v2 = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -vv");
    assert_eq!(v2.verbosity, 2);

    let v3 = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-vvv"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -vvv");
    assert_eq!(v3.verbosity, 3);

    let v4 = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-vvvv"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -vvvv");
    assert_eq!(v4.verbosity, 4);
}

/// Verifies that separate -v flags accumulate (e.g., -v -v -v = 3).
#[test]
fn separate_v_flags_accumulate() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("-v"),
        OsString::from("-v"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -v -v -v");
    assert_eq!(parsed.verbosity, 3);
}

/// Verifies that mixing --verbose and -v accumulates.
#[test]
fn mixed_verbose_flags_accumulate() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("--verbose"),
        OsString::from("-v"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse --verbose -v");
    assert_eq!(parsed.verbosity, 2);
}

// ============================================================================
// --quiet and --no-verbose reset verbosity
// ============================================================================

/// Verifies that --quiet resets verbosity to 0 regardless of preceding -v flags.
#[test]
fn quiet_resets_verbosity_to_zero() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-vvv"),
        OsString::from("--quiet"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -vvv --quiet");
    assert_eq!(parsed.verbosity, 0);
}

/// Verifies that --no-verbose resets verbosity to 0.
#[test]
fn no_verbose_resets_verbosity_to_zero() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--no-verbose"),
        OsString::from("src"),
        OsString::from("dst"),
    ])
    .expect("parse -vv --no-verbose");
    assert_eq!(parsed.verbosity, 0);
}

/// Verifies that --quiet produces no stdout for a file transfer.
#[test]
fn quiet_flag_produces_no_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("quiet_file.txt");
    let destination = tmp.path().join("quiet_file.out");
    std::fs::write(&source, b"quiet mode").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--quiet"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(
        stdout.is_empty(),
        "-v --quiet should produce no stdout, got: {:?}",
        String::from_utf8_lossy(&stdout)
    );
    assert_eq!(std::fs::read(destination).expect("read"), b"quiet mode");
}

// ============================================================================
// Verbose output content increases across levels
// ============================================================================

/// Verifies that higher verbosity levels produce progressively more output.
/// Level 0 < Level 1 < Level 2, measured by stdout byte count.
#[test]
fn higher_verbosity_produces_more_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progressive.txt");
    std::fs::write(&source, b"progressive verbosity test content here").expect("write source");

    // Level 0.
    let dest0 = tmp.path().join("dest0.txt");
    let (code, stdout0, _) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        dest0.into_os_string(),
    ]);
    assert_eq!(code, 0);

    // Level 1.
    let dest1 = tmp.path().join("dest1.txt");
    let (code, stdout1, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.clone().into_os_string(),
        dest1.into_os_string(),
    ]);
    assert_eq!(code, 0);

    // Level 2.
    let dest2 = tmp.path().join("dest2.txt");
    let (code, stdout2, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.into_os_string(),
        dest2.into_os_string(),
    ]);
    assert_eq!(code, 0);

    // Level 0 should have strictly less output than level 1.
    assert!(
        stdout0.len() < stdout1.len(),
        "level 0 ({} bytes) should produce less output than level 1 ({} bytes)",
        stdout0.len(),
        stdout1.len()
    );
    // Level 1 should have strictly less output than level 2.
    assert!(
        stdout1.len() < stdout2.len(),
        "level 1 ({} bytes) should produce less output than level 2 ({} bytes)",
        stdout1.len(),
        stdout2.len()
    );
}

// ============================================================================
// Verbosity combined with -a (archive)
// ============================================================================

#[cfg(unix)]
#[test]
fn verbose_output_includes_symlink_target() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"contents").expect("write source file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let destination_dir = tmp.path().join("dest");
    fs::create_dir(&destination_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        source_dir.into_os_string(),
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    assert!(rendered.contains("link.txt -> file.txt"));
}

// ============================================================================
// Verbosity default value
// ============================================================================

/// Verifies that the default parsed verbosity is 0 when no flags are given.
#[test]
fn default_verbosity_is_zero() {
    let parsed = super::parse_args([
        OsString::from(RSYNC),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse no flags");
    assert_eq!(parsed.verbosity, 0);
}

// ============================================================================
// Verbose with dry-run + delete
// ============================================================================

/// Verifies that -nv --delete lists deletion messages without actually deleting.
#[test]
fn verbose_dry_run_with_delete_lists_deletions_without_removing() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("nv_del_src");
    let dest_dir = tmp.path().join("nv_del_dst");
    fs::create_dir_all(&source_dir).expect("mkdir source");
    fs::create_dir_all(&dest_dir).expect("mkdir dest");

    fs::write(source_dir.join("keep.txt"), b"keep").expect("write keep");
    fs::write(dest_dir.join("keep.txt"), b"old").expect("write dest keep");
    fs::write(dest_dir.join("orphan.txt"), b"orphan").expect("write orphan");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-rnv"),
        OsString::from("--delete"),
        source_operand,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("orphan.txt"),
        "verbose dry-run --delete should mention orphan.txt, got: {rendered:?}"
    );
    // Dry-run: files must remain.
    assert!(
        dest_dir.join("orphan.txt").exists(),
        "dry-run should not actually delete files"
    );
    assert_eq!(
        fs::read(dest_dir.join("orphan.txt")).expect("read"),
        b"orphan"
    );
}

// ============================================================================
// Verbose with --stats and --dry-run
// ============================================================================

/// Verifies that -nv --stats produces both file listing and statistics block
/// without modifying the destination.
#[test]
fn verbose_dry_run_with_stats_shows_statistics() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("drystats.txt");
    let destination = tmp.path().join("drystats.out");
    std::fs::write(&source, b"dry stats content").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-nv"),
        OsString::from("--stats"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );
    let rendered = String::from_utf8(stdout).expect("utf8");
    // File listing from -v.
    assert!(
        rendered.contains("drystats.txt"),
        "dry-run -v --stats should list the file name, got: {rendered:?}"
    );
    // Stats block.
    assert!(
        rendered.contains("Number of files:"),
        "dry-run -v --stats should show stats, got: {rendered:?}"
    );
    // Destination must not be created.
    assert!(
        !destination.exists(),
        "dry-run should not create destination"
    );
}
