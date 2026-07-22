//! Tests verifying that --info=progress2 output format matches upstream rsync.
//!
//! Upstream rsync --info=progress2 format (progress.c:78-134):
//!
//! **Final tick** (per file):
//!   `         10,000 100%    0.50kB/s    0:00:01 (xfr#1, to-chk=2/3)`
//!
//! **In-flight tick**:
//!   `         10,000  50%    0.50kB/s    0:00:01  `
//!
//! The format string `"\r%15s %3d%% %7.2f%s %s%s"` produces:
//!   - Right-aligned bytes (15-char field, thousands-separated)
//!   - Right-aligned percentage (4-char field: ` N%`, `NN%`, `100%`; never `??%`)
//!   - Right-aligned transfer rate (11-char field: value + unit suffix kB/s, MB/s, GB/s)
//!   - Right-aligned time (10-char field: H:MM:SS or ??:??:??)
//!   - Final tick: `(xfr#N, to-chk=M/T)` or `(xfr#N, ir-chk=M/T)` trailer
//!   - In-flight tick: trailing `  ` (two spaces) instead of the xfr trailer
//!
//! These tests validate structural parity by parsing actual progress2 output,
//! allowing numeric value differences while verifying field layout, separators,
//! and unit patterns.

use super::common::*;
use super::*;
use core::client::{
    ClientConfig, ClientProgressObserver, HumanReadableMode, run_client_with_observer,
};

use crate::frontend::progress::{LiveProgress, ProgressMode};

/// Validates that a line matches the upstream progress2 final tick format:
/// `<bytes> <pct> <rate> <time> (xfr#N, {to|ir}-chk=M/T)`
///
/// Checks field presence, separator structure, rate unit, time format,
/// and xfr/chk trailer syntax. Allows numeric value differences.
fn validate_final_tick(line: &str) -> Result<(), String> {
    let trimmed = line.trim();

    // Must contain xfr# trailer
    if !trimmed.contains("xfr#") {
        return Err(format!("final tick missing xfr# trailer: {trimmed:?}"));
    }

    // Must contain to-chk= or ir-chk=
    if !trimmed.contains("to-chk=") && !trimmed.contains("ir-chk=") {
        return Err(format!(
            "final tick missing to-chk= or ir-chk=: {trimmed:?}"
        ));
    }

    // Must contain a percentage (digits followed by %; the percent field never
    // uses a `??` sentinel - progress.c:128).
    if !trimmed.contains('%') {
        return Err(format!("final tick missing percentage: {trimmed:?}"));
    }

    // Must contain a rate unit
    if !trimmed.contains("B/s") {
        return Err(format!("final tick missing rate unit (B/s): {trimmed:?}"));
    }

    // Must contain time in H:MM:SS format (at least two colons in the time field,
    // excluding colons inside the rate unit or chk trailer)
    validate_time_field(trimmed)?;

    // Trailer format: `(xfr#N, {to|ir}-chk=M/T)` - verify parens and comma
    let trailer_start = trimmed
        .rfind("(xfr#")
        .ok_or_else(|| format!("malformed xfr trailer: {trimmed:?}"))?;
    let trailer = &trimmed[trailer_start..];
    if !trailer.ends_with(')') {
        return Err(format!("xfr trailer missing closing paren: {trailer:?}"));
    }
    if !trailer.contains(", ") {
        return Err(format!(
            "xfr trailer missing comma-space separator: {trailer:?}"
        ));
    }

    // Verify chk=M/T contains a slash between two numbers
    let chk_pos = trailer
        .find("-chk=")
        .ok_or_else(|| format!("trailer missing -chk=: {trailer:?}"))?;
    let chk_value = &trailer[chk_pos + 5..trailer.len() - 1]; // strip trailing ')'
    if !chk_value.contains('/') {
        return Err(format!(
            "chk value missing slash (expected M/T): {chk_value:?}"
        ));
    }
    let parts: Vec<&str> = chk_value.split('/').collect();
    if parts.len() != 2 || parts[0].parse::<u64>().is_err() || parts[1].parse::<u64>().is_err() {
        return Err(format!(
            "chk value not in N/M numeric format: {chk_value:?}"
        ));
    }

    Ok(())
}

/// Validates that a line matches the upstream progress2 in-flight tick format.
/// In-flight ticks have the same bytes/pct/rate/time fields but NO xfr trailer.
fn validate_inflight_tick(line: &str) -> Result<(), String> {
    let trimmed = line.trim();

    // Must NOT contain xfr# trailer
    if trimmed.contains("xfr#") {
        return Err(format!(
            "in-flight tick should not contain xfr# trailer: {trimmed:?}"
        ));
    }

    // Must contain a percentage
    if !trimmed.contains('%') {
        return Err(format!("in-flight tick missing percentage: {trimmed:?}"));
    }

    // Must contain a rate unit
    if !trimmed.contains("B/s") {
        return Err(format!("in-flight tick missing rate unit: {trimmed:?}"));
    }

    // Must contain time field
    validate_time_field(trimmed)?;

    Ok(())
}

/// Validates that a time field in H:MM:SS or ??:??:?? format exists in the line.
fn validate_time_field(line: &str) -> Result<(), String> {
    // Look for ??:??:?? sentinel
    if line.contains("??:??:??") {
        return Ok(());
    }

    // Look for H:MM:SS pattern - a digit followed by :DD:DD
    // where DD is a two-digit number
    let bytes = line.as_bytes();
    for i in 0..bytes.len().saturating_sub(6) {
        if bytes[i].is_ascii_digit()
            && i + 1 < bytes.len()
            && bytes[i + 1] == b':'
            && i + 4 < bytes.len()
            && bytes[i + 2].is_ascii_digit()
            && bytes[i + 3].is_ascii_digit()
            && bytes[i + 4] == b':'
            && i + 6 < bytes.len()
            && bytes[i + 5].is_ascii_digit()
            && bytes[i + 6].is_ascii_digit()
        {
            return Ok(());
        }
    }

    Err(format!(
        "line missing H:MM:SS or ??:??:?? time field: {line:?}"
    ))
}

/// Validates that a line matches either the final or in-flight tick format.
fn validate_progress2_line(line: &str) -> Result<(), String> {
    if line.contains("xfr#") {
        validate_final_tick(line)
    } else {
        validate_inflight_tick(line)
    }
}

/// Runs a transfer in progress2 (Overall) mode, returning the raw rendered
/// output as a string.
fn run_progress2_transfer(
    source_dir: &std::path::Path,
    dest_dir: &std::path::Path,
    human_readable: HumanReadableMode,
) -> String {
    let mut source_arg = source_dir.as_os_str().to_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let config = ClientConfig::builder()
        .transfer_args([source_arg, dest_dir.as_os_str().to_os_string()])
        .mkpath(true)
        .progress(true)
        .stats(true)
        .force_event_collection(true)
        .build();

    let mut buffer: Vec<u8> = Vec::new();
    {
        let mut live = LiveProgress::new(&mut buffer, ProgressMode::Overall, human_readable);
        let _summary =
            run_client_with_observer(config, Some(&mut live as &mut dyn ClientProgressObserver))
                .expect("transfer succeeds");
        live.finish().expect("finish succeeds");
    }
    String::from_utf8(buffer).expect("output is valid UTF-8")
}

/// Creates a temp directory with a single source file.
fn setup_single_file(name: &str, size: usize) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    std::fs::write(source_dir.join(name), vec![0xABu8; size]).expect("write source file");
    (tmp, source_dir)
}

/// Creates a temp directory with multiple source files.
fn setup_multiple_files(files: &[(&str, usize)]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).expect("create source dir");
    for (name, size) in files {
        std::fs::write(source_dir.join(name), vec![0xCDu8; *size]).expect("write source file");
    }
    (tmp, source_dir)
}

/// Splits progress2 output into individual lines, handling both `\r` and `\n`
/// as line separators (progress2 uses `\r` for in-place overwrites).
fn split_progress2_lines(output: &str) -> Vec<&str> {
    output
        .split(['\r', '\n'])
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Upstream: progress.c:78-82 - the final tick of the last file emits
/// `(xfr#N, to-chk=0/T)` with a trailing newline.
#[test]
fn progress2_single_file_final_tick_matches_upstream_format() {
    let (tmp, source_dir) = setup_single_file("fmt_check.dat", 2048);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    // There must be at least one final tick with xfr# trailer
    let final_line = lines
        .iter()
        .find(|l| l.contains("xfr#"))
        .expect("progress2 output should contain a final tick with xfr# trailer");

    validate_final_tick(final_line).unwrap_or_else(|e| panic!("{e}"));

    // Final tick should show to-chk=0/2: the trailing-slash copy enumerates the
    // source root directory plus the single file, so `num_files == 2` (reg: 1,
    // dir: 1). upstream: flist.c:2596 counts directories into stats.num_files,
    // so `rsync -r source/ dest` reports `to-chk=0/2` here. Verified against
    // rsync 3.4.4: `Number of files: 2 (reg: 1, dir: 1)`.
    assert!(
        final_line.contains("to-chk=0/2"),
        "single file + dir transfer should end with to-chk=0/2: {final_line:?}"
    );
}

/// Upstream: progress.c:100 - in-flight ticks in progress2 mode use trailing
/// spaces instead of the xfr/chk trailer.
#[test]
fn progress2_inflight_ticks_have_no_xfr_trailer() {
    let (tmp, source_dir) = setup_single_file("inflight.dat", 4 * 1024 * 1024);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    // Collect lines that do NOT contain xfr# (these are in-flight ticks)
    let inflight_lines: Vec<&&str> = lines.iter().filter(|l| !l.contains("xfr#")).collect();

    // In-flight lines (if any) should match the inflight pattern
    for line in &inflight_lines {
        validate_inflight_tick(line).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// Every progress2 line (in-flight or final) must match one of the two
/// upstream format variants.
#[test]
fn progress2_all_lines_match_upstream_format() {
    let (tmp, source_dir) = setup_single_file("all_lines.dat", 2048);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    assert!(
        !lines.is_empty(),
        "progress2 should produce at least one output line"
    );

    for line in &lines {
        validate_progress2_line(line).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// Upstream: progress.c:108-116 - rate field uses base-1024 scaling with
/// `kB/s`, `MB/s`, `GB/s` units. Verify the rate unit is one of these.
#[test]
fn progress2_rate_unit_matches_upstream_tiers() {
    let (tmp, source_dir) = setup_single_file("rate_unit.dat", 4096);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    for line in &lines {
        // Every progress2 line must contain a rate with an upstream unit
        assert!(
            line.contains("kB/s") || line.contains("MB/s") || line.contains("GB/s"),
            "progress2 rate must use upstream base-1024 units (kB/s, MB/s, GB/s): {line:?}"
        );
    }
}

/// Upstream: progress.c:121-122 - time field uses `H:MM:SS` format
/// (`%4u:%02u:%02u`). For a fast transfer, this should be `0:00:0X`.
#[test]
fn progress2_time_field_uses_hmmss_format() {
    let (tmp, source_dir) = setup_single_file("time_fmt.dat", 1024);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    for line in &lines {
        validate_time_field(line).unwrap_or_else(|e| panic!("time field validation failed: {e}"));
    }
}

/// Upstream: progress.c:129 - bytes field uses 15-char right-aligned field
/// with thousands separators.
#[test]
fn progress2_bytes_field_uses_thousands_separator() {
    let (tmp, source_dir) = setup_single_file("sep.dat", 1_536);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);

    // The final tick should contain "1,536" (thousands separator for 1536 bytes)
    assert!(
        output.contains("1,536"),
        "progress2 bytes field should use thousands separator: {output:?}"
    );
}

/// Upstream: progress.c:78-82 - multiple files produce sequential xfr#
/// indices, with the final file showing `to-chk=0/N`.
#[test]
fn progress2_multiple_files_sequential_xfr_indices() {
    let files = [("a.txt", 64), ("b.txt", 128), ("c.txt", 256)];
    let (tmp, source_dir) = setup_multiple_files(&files);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    // Collect all xfr indices from final ticks
    let mut xfr_indices: Vec<u64> = Vec::new();
    for line in &lines {
        if let Some(pos) = line.find("xfr#") {
            let after = &line[pos + 4..];
            let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(idx) = digits.parse::<u64>() {
                xfr_indices.push(idx);
            }
        }
    }

    assert!(
        !xfr_indices.is_empty(),
        "should have at least one xfr# final tick"
    );

    // Indices should be strictly increasing
    for window in xfr_indices.windows(2) {
        assert!(
            window[1] > window[0],
            "xfr indices should be strictly increasing: {} >= {}",
            window[1],
            window[0]
        );
    }

    // The last final tick should show to-chk=0/N (all files done)
    let last_xfr_line = lines
        .iter()
        .filter(|l| l.contains("xfr#"))
        .next_back()
        .expect("should have xfr lines");
    assert!(
        last_xfr_line.contains("to-chk=0/"),
        "last final tick should show to-chk=0/N: {last_xfr_line:?}"
    );
}

/// Validate all final tick lines across a multi-file transfer match
/// the upstream structural format.
#[test]
fn progress2_multiple_files_all_final_ticks_match_format() {
    let files = [("x.bin", 512), ("y.bin", 1024)];
    let (tmp, source_dir) = setup_multiple_files(&files);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::Grouped);
    let lines = split_progress2_lines(&output);

    let final_lines: Vec<&&str> = lines.iter().filter(|l| l.contains("xfr#")).collect();
    assert!(
        !final_lines.is_empty(),
        "should have at least one final tick line"
    );

    for line in &final_lines {
        validate_final_tick(line).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// upstream: progress.c:129 - the bytes field width is 15 chars.
/// Verify `format_progress_bytes` + right-align produces 15-char field.
#[test]
fn progress2_bytes_field_width_15_chars() {
    for &bytes in &[0u64, 42, 1_536, 1_234_567] {
        let formatted = format_progress_bytes(bytes, HumanReadableMode::Grouped);
        let padded = format!("{formatted:>15}");
        assert_eq!(
            padded.len(),
            15,
            "bytes field should be 15 chars for {bytes}: got {padded:?}"
        );
    }
}

/// upstream: progress.c:129 - the percentage field is 4 chars wide.
#[test]
fn progress2_percentage_field_width_4_chars() {
    let test_cases: &[(u64, Option<u64>, &str)] = &[
        (0, Some(100), "  0%"),
        (50, Some(100), " 50%"),
        (100, Some(100), "100%"),
        // upstream progress.c:128 - unknown total resolves to 100%, never `??%`.
        (0, None, "100%"),
    ];

    for (bytes, total, expected) in test_cases {
        let formatted = format_progress_percent(*bytes, *total);
        let padded = format!("{formatted:>4}");
        assert_eq!(
            padded.len(),
            4,
            "percentage field should be 4 chars: got {padded:?}"
        );
        assert_eq!(
            padded, *expected,
            "percentage field mismatch for bytes={bytes}, total={total:?}"
        );
    }
}

/// upstream: progress.c:108-116 - the rate field is 11 chars wide
/// (7-char value + 4-char unit suffix).
#[test]
fn progress2_rate_field_width_11_chars() {
    let test_rates: &[f64] = &[0.0, 512.0, 1_048_576.0, 1_073_741_824.0];
    for &rate in test_rates {
        let formatted = format_progress_rate_decimal(rate);
        let padded = format!("{formatted:>11}");
        assert_eq!(
            padded.len(),
            11,
            "rate field should be 11 chars for rate={rate}: got {padded:?}"
        );
    }
}

/// upstream: progress.c:121-122 - the time field is 10 chars wide
/// (`%4u:%02u:%02u`).
#[test]
fn progress2_time_field_width_10_chars() {
    let test_durations = [
        Duration::ZERO,
        Duration::from_secs(1),
        Duration::from_secs(59),
        Duration::from_secs(3661),
        Duration::from_secs(36_000),
    ];
    for duration in &test_durations {
        let formatted = format_progress_elapsed(*duration);
        let padded = format!("{formatted:>10}");
        assert_eq!(
            padded.len(),
            10,
            "time field should be 10 chars for {duration:?}: got {padded:?}"
        );
    }
}

/// upstream: progress.c:118-119 - overflow sentinel `??:??:??` right-aligned
/// in 10-char field produces `"  ??:??:??"`.
#[test]
fn progress2_time_overflow_sentinel_10_chars() {
    let sentinel = "??:??:??";
    let padded = format!("{sentinel:>10}");
    assert_eq!(padded.len(), 10);
    assert_eq!(padded, "  ??:??:??");
}

/// upstream: progress.c:108-116 - rate units use `kB/s`, `MB/s`, `GB/s`
/// with base-1024 divisors. Verify the tier boundaries.
#[test]
fn progress2_rate_unit_tier_boundaries() {
    // Below 1 MiB/s -> kB/s
    let kb = format_progress_rate_decimal(512.0);
    assert!(kb.ends_with("kB/s"), "512 B/s -> kB/s: {kb}");

    // At 1 MiB/s boundary -> MB/s
    let mb = format_progress_rate_decimal(1024.0 * 1024.0);
    assert!(mb.ends_with("MB/s"), "1 MiB/s -> MB/s: {mb}");

    // At 1 GiB/s boundary -> GB/s
    let gb = format_progress_rate_decimal(1024.0 * 1024.0 * 1024.0);
    assert!(gb.ends_with("GB/s"), "1 GiB/s -> GB/s: {gb}");

    // Well above GB/s stays in GB/s (no TB/s tier in upstream)
    let huge = format_progress_rate_decimal(1024.0 * 1024.0 * 1024.0 * 100.0);
    assert!(huge.ends_with("GB/s"), "100 GiB/s stays GB/s: {huge}");
}

/// Verify the `from_value` variant (used by progress2's sliding-window rate)
/// produces the same unit tiers as the cumulative variant.
#[test]
fn progress2_rate_from_value_matches_cumulative_tiers() {
    for &rate in &[512.0_f64, 1_048_576.0, 1_073_741_824.0] {
        let cumulative = format_progress_rate_decimal(rate);
        let from_value = format_progress_rate_from_value(rate, HumanReadableMode::Grouped);
        assert_eq!(
            cumulative, from_value,
            "from_value should match cumulative for rate={rate}"
        );
    }
}

/// When `--human-readable` is active, bytes field uses K/M/G suffixes
/// instead of thousands separators. The progress2 structure must still
/// match the upstream field layout.
#[test]
fn progress2_human_readable_bytes_use_unit_suffixes() {
    let (tmp, source_dir) = setup_single_file("human.dat", 2_500);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::DecimalUnits);
    let lines = split_progress2_lines(&output);

    assert!(
        !lines.is_empty(),
        "human-readable progress2 should produce output"
    );

    // The bytes field should contain a human-readable unit suffix
    let final_line = lines
        .iter()
        .find(|l| l.contains("xfr#"))
        .expect("should have final tick");
    assert!(
        final_line.contains("K") || final_line.contains("M") || final_line.contains("G"),
        "human-readable bytes should use unit suffixes: {final_line:?}"
    );
}

/// Human-readable mode must still produce lines matching the structural
/// pattern (fields in the same order, separated by spaces).
#[test]
fn progress2_human_readable_structural_parity() {
    let (tmp, source_dir) = setup_single_file("human_struct.dat", 4096);
    let dest_dir = tmp.path().join("dest");

    let output = run_progress2_transfer(&source_dir, &dest_dir, HumanReadableMode::DecimalUnits);
    let lines = split_progress2_lines(&output);

    for line in &lines {
        validate_progress2_line(line).unwrap_or_else(|e| panic!("{e}"));
    }
}

/// Validate that `--info=progress2` through the full CLI path produces
/// lines matching the upstream format.
#[test]
fn progress2_cli_info_flag_produces_upstream_format() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("cli_p2.dat");
    let destination = tmp.path().join("cli_p2.out");
    std::fs::write(&source, vec![0u8; 2048]).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let rendered = String::from_utf8(stdout).expect("utf8");
    let lines = split_progress2_lines(&rendered);

    // At minimum there should be a final tick
    let final_line = lines
        .iter()
        .find(|l| l.contains("xfr#") || l.contains("to-chk="))
        .expect("--info=progress2 should produce at least one final tick line");

    validate_final_tick(final_line).unwrap_or_else(|e| panic!("{e}"));
}

/// Validate that `--info=progress2` with multiple files produces correctly
/// structured output through the CLI path.
#[test]
fn progress2_cli_multiple_files_format_parity() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("multi_src");
    std::fs::create_dir_all(&source_dir).expect("mkdir source");
    std::fs::write(source_dir.join("one.txt"), b"one").expect("write one");
    std::fs::write(source_dir.join("two.txt"), b"two").expect("write two");

    let dest_dir = tmp.path().join("multi_dst");

    let mut source_operand = source_dir.into_os_string();
    source_operand.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        OsString::from("-r"),
        source_operand,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(
        stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&stderr)
    );

    let rendered = String::from_utf8(stdout).expect("utf8");
    let lines = split_progress2_lines(&rendered);

    // Validate all lines match upstream format
    for line in &lines {
        validate_progress2_line(line).unwrap_or_else(|e| panic!("{e}"));
    }

    // Should have at least one final tick
    assert!(
        lines.iter().any(|l| l.contains("xfr#")),
        "multi-file progress2 should have xfr# final ticks: {rendered:?}"
    );
}

/// Verifies that the composed progress2 line (bytes + pct + rate + time +
/// trailer) produces the correct field order and separators when assembled
/// from the individual formatter functions, matching upstream's format
/// string: `"\r%15s %3d%% %7.2f%s %s%s"` (progress.c:129).
///
/// upstream: progress.c:108-134 rprint_progress
#[test]
fn progress2_composed_line_field_order_matches_upstream() {
    let bytes_field = format!(
        "{:>15}",
        format_progress_bytes(1_536, HumanReadableMode::Grouped)
    );
    let percent_field = format!("{:>4}", format_progress_percent(1_536, Some(1_536)));
    let rate_field = format!(
        "{:>11}",
        format_progress_rate(1_536, Duration::from_secs(1), HumanReadableMode::Grouped)
    );
    let time_field = format!("{:>10}", format_progress_elapsed(Duration::from_secs(1)));

    // Compose a final tick line
    let line =
        format!("{bytes_field} {percent_field} {rate_field} {time_field} (xfr#1, to-chk=0/1)");

    // Verify field widths
    assert_eq!(bytes_field.len(), 15, "bytes field width");
    assert_eq!(percent_field.len(), 4, "percent field width");
    assert_eq!(rate_field.len(), 11, "rate field width");
    assert_eq!(time_field.len(), 10, "time field width");

    // Validate the composed line matches upstream format
    validate_final_tick(&line).unwrap_or_else(|e| panic!("{e}"));

    // Verify specific content
    assert!(line.contains("1,536"), "bytes field should contain 1,536");
    assert!(line.contains("100%"), "should show 100%");
    assert!(line.contains("kB/s"), "rate should use kB/s");
    assert!(line.contains("0:00:01"), "time should be 0:00:01");
    assert!(
        line.contains("(xfr#1, to-chk=0/1)"),
        "trailer should match upstream format"
    );
}

/// Verifies that an in-flight progress2 line (no xfr trailer, trailing spaces)
/// has the correct field order when assembled from formatter functions.
///
/// upstream: progress.c:100 - in-flight ticks emit trailing spaces
#[test]
fn progress2_composed_inflight_line_field_order_matches_upstream() {
    let bytes_field = format!(
        "{:>15}",
        format_progress_bytes(512, HumanReadableMode::Grouped)
    );
    let percent_field = format!("{:>4}", format_progress_percent(512, Some(1_024)));
    let rate_field = format!(
        "{:>11}",
        format_progress_rate(512, Duration::from_millis(500), HumanReadableMode::Grouped)
    );
    let time_field = format!(
        "{:>10}",
        format_progress_elapsed(Duration::from_millis(500))
    );

    // In-flight line ends with two trailing spaces
    let line = format!("{bytes_field} {percent_field} {rate_field} {time_field}  ");

    validate_inflight_tick(&line).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !line.contains("xfr#"),
        "in-flight line should not have xfr trailer"
    );
}
