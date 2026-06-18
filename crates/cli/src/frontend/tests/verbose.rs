use super::common::*;
use super::*;

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
    assert!(std::fs::symlink_metadata(&destination).is_err());

    // The upstream-format NONREG notice (generator.c:1687) must appear on
    // either stream: emit_verbose renders it to stdout at -v, and the
    // default-on `--info=NONREG` emission feeds the diagnostic queue.
    let stdout_text = String::from_utf8(stdout).expect("verbose stdout is UTF-8");
    let stderr_text = String::from_utf8(stderr).expect("verbose stderr is UTF-8");
    let notice = "skipping non-regular file \"skip.pipe\"";
    assert!(
        stdout_text.contains(notice) || stderr_text.contains(notice),
        "expected NONREG notice on stdout or stderr\nstdout: {stdout_text:?}\nstderr: {stderr_text:?}"
    );
}

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

/// Verifies that -vv lists files using upstream's bare `%n%L` format
/// (no `copied:`/`symlink:` descriptor prefix, no byte-count wrapper).
/// Upstream: options.c:2372 sets `stdout_format = "%n%L"`; log.c:603-659
/// expands `%n` to the filename and `%L` to ` -> target` for symlinks.
/// The upstream testsuite `duplicates.test` greps `^name1$` to detect
/// duplicate copies, so the per-file line must be bare.
#[test]
fn level_2_emits_bare_name_per_upstream() {
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
    assert!(
        rendered.lines().any(|line| line == "descriptor.txt"),
        "level 2 must emit bare `<name>` per upstream `%n%L`, got: {rendered:?}"
    );
    for forbidden in ["copied:", "symlink:", "hard link:", "directory:"] {
        assert!(
            !rendered.contains(forbidden),
            "level 2 must not emit `{forbidden}` descriptor prefix, got: {rendered:?}"
        );
    }
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
    // -vvv routes debug_log! output to stderr, so only error-level lines
    // ("rsync error:" / "rsync: error") indicate a real failure here.
    let stderr_str = String::from_utf8_lossy(&stderr);
    assert!(
        !stderr_str.contains("rsync error:") && !stderr_str.contains("rsync: error"),
        "stderr should contain no errors, got: {stderr_str:?}"
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
    assert!(
        rendered.contains("vstats.txt"),
        "verbose --stats should list the file name, got: {rendered:?}"
    );
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
    assert!(
        rendered.contains("Number of files:"),
        "--stats should always show statistics block, got: {rendered:?}"
    );
    assert!(
        rendered.contains("Total file size:"),
        "--stats should always show Total file size, got: {rendered:?}"
    );
}

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
    // upstream: -v --delete prints "deleting <file>" lines.
    assert!(
        rendered.contains("orphan.txt"),
        "verbose --delete should mention the deleted file, got: {rendered:?}"
    );
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
    assert!(
        stdout.is_empty(),
        "delete without verbose should produce no stdout, got: {:?}",
        String::from_utf8_lossy(&stdout)
    );
    assert!(
        !dest_dir.join("stale.txt").exists(),
        "stale.txt should have been deleted"
    );
}

/// Verifies that --verbose produces the same output shape as -v.
#[test]
fn long_verbose_flag_equivalent_to_short() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("equiv.txt");
    std::fs::write(&source, b"equivalence test").expect("write source");

    let dest_short = tmp.path().join("short.out");
    let (code_short, stdout_short, stderr_short) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.clone().into_os_string(),
        dest_short.into_os_string(),
    ]);

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

    assert!(
        rendered_short.contains("equiv.txt"),
        "-v should show equiv.txt"
    );
    assert!(
        rendered_long.contains("equiv.txt"),
        "--verbose should show equiv.txt"
    );

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

/// Verifies that higher verbosity levels never drop output.
///
/// Level 0 emits nothing per upstream; Level 1 begins per-file `%n%L`
/// (upstream `log.c::log_formatted` lines 633-659, triggered when
/// `INFO_GTE(NAME, 1)` per `options.c::set_output_verbosity`). Level 2
/// only adds further output for events that fire conditionally - it does
/// NOT extend the per-file line with a descriptor prefix or rate-display
/// wrapper. For a single-file success transfer through the local-copy
/// executor (no `--stats`, no skipped files), `-v` and `-vv` therefore
/// emit identical byte counts. The earlier strict-less assertion locked
/// in the pre-bare-render divergence and started panicking after that
/// renderer was aligned with upstream.
#[test]
fn higher_verbosity_produces_more_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progressive.txt");
    std::fs::write(&source, b"progressive verbosity test content here").expect("write source");

    let dest0 = tmp.path().join("dest0.txt");
    let (code, stdout0, _) = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        dest0.into_os_string(),
    ]);
    assert_eq!(code, 0);

    let dest1 = tmp.path().join("dest1.txt");
    let (code, stdout1, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.clone().into_os_string(),
        dest1.into_os_string(),
    ]);
    assert_eq!(code, 0);

    let dest2 = tmp.path().join("dest2.txt");
    let (code, stdout2, _) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.into_os_string(),
        dest2.into_os_string(),
    ]);
    assert_eq!(code, 0);

    assert!(
        stdout0.len() < stdout1.len(),
        "level 0 ({} bytes) should produce less output than level 1 ({} bytes)",
        stdout0.len(),
        stdout1.len()
    );
    assert!(
        stdout1.len() <= stdout2.len(),
        "level 2 ({} bytes) must not drop output relative to level 1 ({} bytes)",
        stdout2.len(),
        stdout1.len()
    );
}

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
    assert!(
        dest_dir.join("orphan.txt").exists(),
        "dry-run should not actually delete files"
    );
    assert_eq!(
        fs::read(dest_dir.join("orphan.txt")).expect("read"),
        b"orphan"
    );
}

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
    assert!(
        rendered.contains("drystats.txt"),
        "dry-run -v --stats should list the file name, got: {rendered:?}"
    );
    assert!(
        rendered.contains("Number of files:"),
        "dry-run -v --stats should show stats, got: {rendered:?}"
    );
    assert!(
        !destination.exists(),
        "dry-run should not create destination"
    );
}

/// Mirrors the upstream `testsuite/duplicates.test` scenario: the same
/// source directory passed multiple times must produce exactly one
/// bare `name1` line and exactly one `name2 -> target` line on -vv stdout.
/// The test greps `^name1$` / `^name2 -> ` to detect duplicate copies, so
/// any prefix (`copied:`, `symlink:`) or byte-count wrapper breaks interop.
/// Upstream: `testsuite/duplicates.test`, options.c:2372 (`stdout_format = "%n%L"`).
#[cfg(unix)]
#[test]
fn duplicates_testsuite_emits_bare_name_lines() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    fs::create_dir(&from).expect("create from");
    fs::create_dir(&to).expect("create to");

    let name1 = from.join("name1");
    let name2 = from.join("name2");
    fs::write(&name1, b"This is the file\n").expect("write name1");
    symlink(&name1, &name2).expect("create symlink");

    let mut from_arg = from.clone().into_os_string();
    from_arg.push("/");
    let mut argv = vec![OsString::from(RSYNC), OsString::from("-avv")];
    for _ in 0..10 {
        argv.push(from_arg.clone());
    }
    argv.push(to.into_os_string());

    let (code, stdout, _stderr) = run_with_args(argv);
    assert_eq!(code, 0, "duplicates transfer should succeed");

    let rendered = String::from_utf8(stdout).expect("utf8");
    let name1_lines = rendered.lines().filter(|line| *line == "name1").count();
    let name2_lines = rendered
        .lines()
        .filter(|line| line.starts_with("name2 -> "))
        .count();
    assert_eq!(
        name1_lines, 1,
        "name1 must appear exactly once as a bare line, got: {rendered:?}"
    );
    assert_eq!(
        name2_lines, 1,
        "name2 must appear exactly once as `name2 -> ...`, got: {rendered:?}"
    );
}

/// Verifies that `-vv` on an all-uptodate tree mirrors upstream rsync 3.4.4:
///
/// 1. The first stdout line is `sending incremental file list` (flist.c:2252).
/// 2. Unchanged files emit `<name> is uptodate` (rsync.c:676) at NAME>=2.
/// 3. Files are NOT pre-listed as bare names before the uptodate notice -
///    upstream gates the bare per-file emission on `INFO_EQ(PROGRESS, 1)`
///    (receiver.c:1011, sender.c:450), and no `--progress` was requested here.
#[test]
fn level_2_all_uptodate_matches_upstream_banner_and_uptodate_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    std::fs::create_dir_all(from.join("foo")).expect("mkdir from/foo");
    std::fs::write(from.join("foo").join("a.txt"), b"alpha").expect("write a");
    std::fs::write(from.join("foo").join("b.txt"), b"bravo").expect("write b");

    let mut from_slash = from.clone().into_os_string();
    from_slash.push("/");

    // Seed the destination so the second invocation finds everything uptodate.
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-tplr"),
        from_slash.clone(),
        to.clone().into_os_string(),
    ]);
    assert_eq!(code, 0, "seeding initial sync must succeed");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vvplrH"),
        from_slash,
        to.into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {:?}",
        String::from_utf8_lossy(&stderr)
    );

    let rendered = String::from_utf8(stdout).expect("verbose stdout utf8");
    let lines: Vec<&str> = rendered.lines().collect();

    assert!(
        !lines.is_empty(),
        "expected at least the `sending incremental file list` banner, got: {rendered:?}"
    );
    assert_eq!(
        lines[0], "sending incremental file list",
        "first line must be the FCLIENT banner (flist.c:2252), got: {rendered:?}"
    );

    // Per upstream rsync.c:676 + sender.c:450, no bare `<name>` lines should
    // precede the `is uptodate` notice when --progress is absent. A regression
    // would surface as e.g. `foo/a.txt` on its own line right after the banner.
    let uptodate_count = lines.iter().filter(|l| l.ends_with(" is uptodate")).count();
    assert_eq!(
        uptodate_count, 2,
        "expected `is uptodate` for each unchanged file, got: {rendered:?}"
    );

    let bare_path_count = lines
        .iter()
        .filter(|l| *l == &"foo/a.txt" || *l == &"foo/b.txt")
        .count();
    assert_eq!(
        bare_path_count, 0,
        "unchanged files must NOT emit a bare-name line before their `is uptodate` \
         notice (upstream gates this on INFO_EQ(PROGRESS, 1)), got: {rendered:?}"
    );

    // Summary still appears.
    assert!(
        rendered.contains("sent "),
        "expected sent/received summary line, got: {rendered:?}"
    );
}

/// Verifies that `-v` (verbose=1) on an all-uptodate tree emits the banner
/// but suppresses both `is uptodate` notices (which need NAME>=2) and bare
/// per-file lines (which need `--progress`).
#[test]
fn level_1_all_uptodate_emits_banner_only() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    std::fs::create_dir_all(&from).expect("mkdir from");
    std::fs::write(from.join("only.txt"), b"only").expect("write");

    let mut from_slash = from.clone().into_os_string();
    from_slash.push("/");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-tplr"),
        from_slash.clone(),
        to.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vplrH"),
        from_slash,
        to.into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("sending incremental file list"),
        "first line must be the FCLIENT banner at -v, got: {rendered:?}"
    );
    assert!(
        !rendered.contains("is uptodate"),
        "-v (NAME<2) must NOT emit `is uptodate` lines, got: {rendered:?}"
    );
    assert!(
        !lines.contains(&"only.txt"),
        "unchanged file must NOT emit a bare-name line under -v without --progress, got: {rendered:?}"
    );
}

/// Verifies that `-vvplrH` against an already-hardlinked destination emits the
/// upstream `is uptodate` notice for the hardlink companion instead of the
/// bare path.
///
/// upstream: hlink.c:218-224 - when the destination already shares the source
/// group leader's inode, the generator emits `"%s is uptodate"` at
/// `INFO_GTE(NAME, 2) && maybe_ATTRS_REPORT`. Mirrors `testsuite/itemize.test`
/// at line 106 (`foo/extra is uptodate`).
#[cfg(unix)]
#[test]
fn level_2_hardlink_uptodate_emits_is_uptodate_notice() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    std::fs::create_dir_all(&from).expect("mkdir from");
    std::fs::write(from.join("leader.txt"), b"leader content").expect("write leader");
    std::fs::hard_link(from.join("leader.txt"), from.join("follower.txt"))
        .expect("source hardlink");

    let mut from_slash = from.clone().into_os_string();
    from_slash.push("/");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-plrH"),
        from_slash.clone(),
        to.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vvplrH"),
        from_slash,
        to.into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        rendered.contains("leader.txt is uptodate"),
        "leader must emit `is uptodate` under -vv, got: {rendered:?}"
    );
    assert!(
        rendered.contains("follower.txt is uptodate"),
        "hardlink follower must emit `is uptodate` under -vv (upstream hlink.c:218-224), got: {rendered:?}"
    );
    assert!(
        !rendered.lines().any(|line| line == "follower.txt"),
        "hardlink follower must not emit the bare path under -vv, got: {rendered:?}"
    );
}

/// Verifies that `-vvplr` (no `--stats`) emits a blank line separating the
/// per-file listing from the totals trailer so the upstream
/// `testsuite/itemize.test` `v_filt` helper (`sed -e '/^$/,$d'`) can strip the
/// trailer before diffing.
///
/// upstream: main.c:461 - `output_summary()` emits `rprintf(FCLIENT, "\n")`
/// before the `INFO_GTE(STATS, 1)` totals block.
#[test]
fn level_2_blank_line_precedes_totals_trailer() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    std::fs::create_dir_all(&from).expect("mkdir from");
    std::fs::write(from.join("trailer.txt"), b"separator").expect("write source");

    let mut from_slash = from.clone().into_os_string();
    from_slash.push("/");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vvplr"),
        from_slash,
        to.into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let lines: Vec<&str> = rendered.lines().collect();
    let trailer_index = lines
        .iter()
        .position(|line| line.starts_with("sent "))
        .expect("totals trailer must be present");
    assert!(
        trailer_index > 0,
        "totals trailer must follow at least one listing line, got: {rendered:?}"
    );
    assert!(
        lines[trailer_index - 1].is_empty(),
        "an empty line must precede the totals trailer so v_filt strips it correctly, got: {rendered:?}"
    );
}

/// Verifies that under `-vv` without `-i`, uptodate notices are emitted
/// before transferred-file lines so the observable wire order matches
/// upstream's pipelined generator-first / receiver-second emission.
///
/// upstream: generator.c emits `"is uptodate"` synchronously while the
/// receiver emits the bare-name notice from `set_file_attrs` (rsync.c:672-676)
/// only after the transfer completes. Mirrors `testsuite/itemize.test`
/// lines 102-109 which expect uptodate notices to precede the transferred
/// `foo/config2` line even though `foo/config2` precedes `foo/extra` and
/// `foo/sym` alphabetically.
#[test]
fn level_2_uptodate_lines_precede_transferred_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    std::fs::create_dir_all(&from).expect("mkdir from");
    std::fs::write(from.join("aaa.txt"), b"alpha content").expect("write aaa");
    std::fs::write(from.join("bbb.txt"), b"bravo content").expect("write bbb");
    std::fs::write(from.join("zzz.txt"), b"zulu content").expect("write zzz");

    let mut from_slash = from.clone().into_os_string();
    from_slash.push("/");

    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-tplr"),
        from_slash.clone(),
        to.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);

    // Rewrite only `aaa.txt` so that the next run transfers `aaa.txt` while
    // `bbb.txt` and `zzz.txt` stay uptodate.
    std::fs::write(from.join("aaa.txt"), b"alpha modified content").expect("rewrite aaa");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vvplr"),
        from_slash,
        to.into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    let lines: Vec<&str> = rendered.lines().collect();
    let bbb_idx = lines
        .iter()
        .position(|l| *l == "bbb.txt is uptodate")
        .unwrap_or_else(|| panic!("missing `bbb.txt is uptodate`: {rendered:?}"));
    let zzz_idx = lines
        .iter()
        .position(|l| *l == "zzz.txt is uptodate")
        .unwrap_or_else(|| panic!("missing `zzz.txt is uptodate`: {rendered:?}"));
    let aaa_idx = lines
        .iter()
        .position(|l| *l == "aaa.txt")
        .unwrap_or_else(|| panic!("missing transferred `aaa.txt`: {rendered:?}"));
    assert!(
        bbb_idx < aaa_idx && zzz_idx < aaa_idx,
        "uptodate notices must precede transferred files (upstream generator/receiver pipeline), got: {rendered:?}"
    );
}

/// Verifies that `-iplrtH` against a destination already linked to the leader
/// emits the `hf` itemized row WITHOUT the `=> leader` suffix.
///
/// upstream: hlink.c:218-222 - when the destination already shares the source
/// group leader's inode, `maybe_hard_link()` calls
/// `itemize(..., ITEM_LOCAL_CHANGE | ITEM_XNAME_FOLLOWS, 0, "")` with an
/// empty xname. `log.c:643-654` skips the `%L` ` => %s` suffix when the
/// xname is empty. Mirrors `testsuite/itemize.test` at line 122
/// (`hf$allspace foo/extra`) which has no `=>` suffix because the dest
/// alias is already linked to `foo/config1`.
#[cfg(unix)]
#[test]
fn itemize_hardlink_already_linked_omits_arrow_suffix() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    std::fs::create_dir_all(&from).expect("mkdir from");
    std::fs::write(from.join("leader.bin"), b"shared content").expect("write leader");
    std::fs::hard_link(from.join("leader.bin"), from.join("follower.bin"))
        .expect("source hardlink");

    let mut from_slash = from.clone().into_os_string();
    from_slash.push("/");

    // First pass: create the destination alias.
    let (code, _stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-plrtH"),
        from_slash.clone(),
        to.clone().into_os_string(),
    ]);
    assert_eq!(code, 0);

    // Second pass: destination is already linked. Itemize must not emit
    // the `=> leader.bin` xname suffix on the follower row.
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-iplrtH"),
        from_slash,
        to.into_os_string(),
    ]);
    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8");
    assert!(
        !rendered.contains("=> leader.bin"),
        "already-linked hardlink row must omit `=> leader.bin` (upstream hlink.c:218-222 passes empty xname to itemize), got: {rendered:?}"
    );
}
