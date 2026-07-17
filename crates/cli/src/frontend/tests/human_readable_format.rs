use super::common::*;
use super::*;

#[test]
fn human_readable_formats_kilobytes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("1k.bin");
    std::fs::write(&source, vec![0u8; 1_024]).expect("write source");

    let dest = tmp.path().join("1k.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(
        rendered.contains("1.02K"),
        "expected K suffix, got: {rendered}"
    );
}

#[test]
fn human_readable_formats_megabytes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("1m.bin");
    std::fs::write(&source, vec![0u8; 1_048_576]).expect("write source");

    let dest = tmp.path().join("1m.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(
        rendered.contains("1.05M"),
        "expected M suffix, got: {rendered}"
    );
}

#[test]
fn human_readable_formats_gigabytes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("1g.bin");
    let file = std::fs::File::create(&source).expect("create source");
    let size: u64 = 1_073_741_824;
    file.set_len(size).expect("extend source to 1GB");

    let dest = tmp.path().join("1g.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        OsString::from("--sparse"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(
        rendered.contains("1.07G"),
        "expected G suffix, got: {rendered}"
    );
}

#[test]
fn human_readable_formats_various_sizes() {
    use tempfile::tempdir;

    let test_cases = vec![
        (512, "512"),           // Below 1K
        (1_500, "1.50K"),       // 1.5K
        (10_240, "10.24K"),     // 10K
        (100_000, "100.00K"),   // 100K
        (1_500_000, "1.50M"),   // 1.5M
        (50_000_000, "50.00M"), // 50M
    ];

    for (size, expected) in test_cases {
        let tmp = tempdir().expect("tempdir");
        let source = tmp.path().join("test.bin");
        std::fs::write(&source, vec![0u8; size]).expect("write source");

        let dest = tmp.path().join("test.out");
        let (code, stdout, stderr) = run_with_args([
            OsString::from(RSYNC),
            OsString::from("--stats"),
            OsString::from("-h"),
            source.into_os_string(),
            dest.into_os_string(),
        ]);

        assert_eq!(code, 0);
        assert!(stderr.is_empty());
        let rendered = String::from_utf8(stdout).expect("stats output utf8");
        assert!(
            rendered.contains(expected),
            "expected {expected} for size {size}, got: {rendered}"
        );
    }
}

#[test]
fn multiple_h_flags_enable_combined_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-h"),
        OsString::from("-h"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::BinaryUnits));
}

#[test]
fn three_h_flags_remain_combined_mode() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("-h"),
        OsString::from("-h"),
        OsString::from("-h"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");

    assert_eq!(parsed.human_readable, Some(HumanReadableMode::BinaryUnits));
}

#[test]
fn combined_mode_uses_base_1024_no_exact() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("combined.bin");
    std::fs::write(&source, vec![0u8; 2_048]).expect("write source");

    let dest = tmp.path().join("combined.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    // upstream: lib/compat.c:183 - `-hh` divides by 1024 (2048 / 1024 = 2.00K)
    // and never appends an exact-value component.
    assert!(
        rendered.contains("2.00K") && !rendered.contains("(2,048)"),
        "expected base-1024 format without exact component, got: {rendered}"
    );
}

#[test]
fn combined_mode_long_form_uses_base_1024() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("test.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest = tmp.path().join("test.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-hh"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    // upstream: lib/compat.c:183 - `-hh` divides by 1024 (1536 / 1024 = 1.50K)
    // and never appends an exact-value component.
    assert!(
        rendered.contains("1.50K") && !rendered.contains("(1,536)"),
        "expected base-1024 format without exact component, got: {rendered}"
    );
}

#[test]
fn human_readable_output_format_matches_upstream() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("upstream.bin");
    std::fs::write(&source, vec![0u8; 5_120]).expect("write source");

    let dest = tmp.path().join("upstream.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");

    // upstream: stats use 2 decimal places (e.g. "5.12K").
    assert!(rendered.contains("5.12K"));

    let has_total_file_size = rendered
        .lines()
        .any(|line| line.starts_with("Total file size:") && line.contains("5.12K"));
    assert!(
        has_total_file_size,
        "expected 'Total file size: 5.12K bytes' line, got: {rendered}"
    );
}

#[test]
fn human_readable_uses_two_decimal_places() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("decimal.bin");
    std::fs::write(&source, vec![0u8; 1_234]).expect("write source");

    let dest = tmp.path().join("decimal.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(
        rendered.contains("1.23K"),
        "expected two decimal places (1.23K), got: {rendered}"
    );
}

#[test]
fn human_readable_formats_bytes_without_suffix() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("small.bin");
    std::fs::write(&source, vec![0u8; 512]).expect("write source");

    let dest = tmp.path().join("small.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(
        rendered.contains("Total file size: 512 bytes"),
        "expected plain bytes, got: {rendered}"
    );
}

#[test]
fn human_readable_works_with_stats() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("stats.bin");
    std::fs::write(&source, vec![0u8; 2_048]).expect("write source");

    let dest = tmp.path().join("stats.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");

    assert!(rendered.contains("Number of files:"));
    assert!(rendered.contains("Total file size:"));
    assert!(rendered.contains("Total bytes sent:"));

    assert!(rendered.contains("2.05K"));
}

#[test]
fn human_readable_works_with_progress() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progress.bin");
    std::fs::write(&source, vec![0u8; 3_072]).expect("write source");

    let dest = tmp.path().join("progress.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");

    let normalized = rendered.replace('\r', "\n");
    assert!(
        normalized.contains("3.07K"),
        "expected human-readable size in progress, got: {rendered}"
    );

    assert!(normalized.contains("progress.bin"));
    assert!(normalized.contains("(xfr#1"));
}

#[test]
fn human_readable_works_with_stats_and_progress() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("both.bin");
    std::fs::write(&source, vec![0u8; 4_096]).expect("write source");

    let dest = tmp.path().join("both.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("--progress"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("output utf8");

    assert!(rendered.contains("both.bin"));
    assert!(rendered.contains("(xfr#1"));
    assert!(rendered.contains("Number of files:"));
    assert!(rendered.contains("Total file size:"));

    assert!(
        rendered.contains("4.10K"),
        "expected human-readable size, got: {rendered}"
    );
}

#[test]
fn human_readable_combined_works_with_stats() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("combined-stats.bin");
    std::fs::write(&source, vec![0u8; 8_192]).expect("write source");

    let dest = tmp.path().join("combined-stats.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-hh"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");

    // upstream: lib/compat.c:183 - `-hh` divides by 1024 (8192/1024 = 8.00K)
    // and never appends an exact-value component.
    assert!(
        rendered.contains("8.00K") && !rendered.contains("(8,192)"),
        "expected base-1024 format without exact component in stats, got: {rendered}"
    );
    assert!(rendered.contains("Number of files:"));
}

#[test]
fn human_readable_combined_works_with_progress() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("combined-progress.bin");
    std::fs::write(&source, vec![0u8; 6_144]).expect("write source");

    let dest = tmp.path().join("combined-progress.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("-hh"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");

    let normalized = rendered.replace('\r', "\n");
    // upstream: `-hh` divides by 1024 (6144/1024 = 6.00K), no exact component.
    assert!(
        normalized.contains("6.00K") && !normalized.contains("(6,144)"),
        "expected base-1024 format without exact component in progress, got: {rendered}"
    );
}

#[test]
fn human_readable_handles_zero_bytes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("empty.bin");
    std::fs::write(&source, vec![]).expect("write empty source");

    let dest = tmp.path().join("empty.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");
    assert!(
        rendered.contains("Total file size: 0 bytes"),
        "expected zero bytes, got: {rendered}"
    );
}

#[test]
fn human_readable_with_verbose_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("verbose.bin");
    std::fs::write(&source, vec![0u8; 10_240]).expect("write source");

    let dest = tmp.path().join("verbose.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("output utf8");

    assert!(rendered.contains("verbose.bin"));
    assert!(rendered.contains("10.24K"));
    assert!(rendered.contains("\n\nsent"));
}

#[test]
fn no_human_readable_shows_raw_digits() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("exact.bin");
    std::fs::write(&source, vec![0u8; 1_234_567]).expect("write source");

    let dest = tmp.path().join("exact.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("--no-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");

    // upstream: --no-h => level 0 (do_big_num flag 0) emits raw digits with no
    // thousands separators. `rsync --stats --no-h` prints `Total file size:
    // 1234567 bytes`, not `1,234,567`.
    assert!(
        rendered.contains("Total file size: 1234567 bytes"),
        "expected raw digits without separators, got: {rendered}"
    );
    assert!(
        !rendered.contains("1,234,567"),
        "level 0 must not group digits with commas, got: {rendered}"
    );
    assert!(
        !rendered.contains("1.18M") && !rendered.contains("1.23M"),
        "level 0 must not use a unit suffix, got: {rendered}"
    );
}

/// Renders `--stats` over a >1000-file tree at each human-readable level and
/// pins the COUNT fields (not byte sizes). upstream: main.c output_summary wraps
/// every count in comma_num = do_big_num(num, human_readable != 0, NULL)
/// (inums.h): raw digits under --no-h (level 0), comma-grouped at every enabled
/// level (1/2/3), and never humanised to K/M/G units - so -hh groups counts but
/// keeps `1,501`, unlike byte sizes. oc previously grouped counts
/// unconditionally, mis-rendering `--no-h` counts as `1,501`.
fn stats_counts_for_level(flag: Option<&str>) -> String {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("tree");
    std::fs::create_dir(&source).expect("mkdir source");
    for i in 0..1_500 {
        std::fs::write(source.join(format!("f{i}")), b"x").expect("write file");
    }

    let dest = tmp.path().join("out");
    let mut args = vec![
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-r"),
    ];
    if let Some(flag) = flag {
        args.push(OsString::from(flag));
    }
    let mut src = source.into_os_string();
    src.push("/");
    args.push(src);
    args.push(dest.into_os_string());

    let (code, stdout, stderr) = run_with_args(args);
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    assert!(stderr.is_empty());
    String::from_utf8(stdout).expect("stats output utf8")
}

#[test]
fn stats_counts_raw_digits_under_no_h() {
    // upstream real binary, fresh dest: both the total and the created count
    // (every entry incl. the top dir is new, so created == total) render raw.
    //   Number of files:         1501 (reg: 1500, dir: 1)
    //   Number of created files: 1501 (reg: 1500, dir: 1)
    let rendered = stats_counts_for_level(Some("--no-h"));
    assert!(
        rendered.contains("Number of files: 1501 (reg: 1500, dir: 1)"),
        "level 0 counts must be raw digits, got: {rendered}"
    );
    assert!(
        rendered.contains("Number of created files: 1501 (reg: 1500, dir: 1)"),
        "level 0 created count must be raw and == total, got: {rendered}"
    );
    assert!(
        !rendered.contains("1,50"),
        "level 0 must not group any count, got: {rendered}"
    );
}

#[test]
fn stats_counts_grouped_at_default_level() {
    // upstream real binary, fresh dest: created == total (all entries new), both
    // comma-grouped: `Number of {files,created files}: 1,501 (reg: 1,500, dir: 1)`.
    let rendered = stats_counts_for_level(None);
    assert!(
        rendered.contains("Number of files: 1,501 (reg: 1,500, dir: 1)"),
        "default level counts must be comma-grouped, got: {rendered}"
    );
    assert!(
        rendered.contains("Number of created files: 1,501 (reg: 1,500, dir: 1)"),
        "default created count must be grouped and == total, got: {rendered}"
    );
}

#[test]
fn stats_counts_grouped_not_humanised_at_hh() {
    // upstream real binary, fresh dest: `-hh` comma-groups counts (comma_num
    // passes human_readable != 0 == 1) but NEVER humanises them to K/M/G units,
    // while byte sizes (human_num) ARE humanised at the same level. That
    // size-vs-count split is exactly what #171 verifies.
    let rendered = stats_counts_for_level(Some("-hh"));
    assert!(
        rendered.contains("Number of files: 1,501 (reg: 1,500, dir: 1)"),
        "-hh counts must be comma-grouped, not humanised, got: {rendered}"
    );
    assert!(
        rendered.contains("Number of created files: 1,501 (reg: 1,500, dir: 1)"),
        "-hh created count must be comma-grouped, not humanised, got: {rendered}"
    );
    // Byte sizes ARE humanised at -hh: 1500 bytes / 1024 = 1.46K. Its presence
    // confirms the level is active and only counts are exempt from unit suffixes.
    assert!(
        rendered.contains("Total file size: 1.46K bytes"),
        "-hh must humanise byte sizes (proving counts are the exception), got: {rendered}"
    );
}

#[test]
fn human_readable_format_consistency_across_stats() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("consistent.bin");
    std::fs::write(&source, vec![0u8; 20_480]).expect("write source");

    let dest = tmp.path().join("consistent.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--stats"),
        OsString::from("-h"),
        source.into_os_string(),
        dest.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stats output utf8");

    let count_k = rendered.matches("20.48K").count();
    assert!(
        count_k >= 2,
        "expected human-readable format used consistently, got: {rendered}"
    );
}
