use super::common::*;
use super::*;

#[test]
fn parse_args_recognises_block_size_argument() {
    let parsed = parse_args([
        OsString::from(RSYNC),
        OsString::from("--block-size=16384"),
        OsString::from("source"),
        OsString::from("dest"),
    ])
    .expect("parse");
    assert_eq!(parsed.block_size, Some(OsString::from("16384")));
}

/// The argv layer carries the value through verbatim; it is not the validator.
///
/// upstream: options.c:1802 hands the raw string to `parse_size_arg`, which
/// owns the whole grammar - `1K`, `1KB`, `1KiB`, `1.5K`, `1K+1`. A bare-integer
/// check here would reject all of those before the real parser ran, which is
/// exactly the defect this replaces: `--min-size` and `--max-size`, parsed by
/// the same helper one layer down, accepted them while `--block-size` did not.
#[test]
fn parse_args_carries_suffixed_block_size_through_to_the_size_parser() {
    for value in ["1K", "1KiB", "1.5K", "1K+1", "128K"] {
        let parsed = parse_args([
            OsString::from(RSYNC),
            OsString::from(format!("--block-size={value}")),
            OsString::from("source"),
            OsString::from("dest"),
        ])
        .unwrap_or_else(|error| panic!("--block-size={value} must reach the size parser: {error}"));
        assert_eq!(parsed.block_size, Some(OsString::from(value)));
    }
}

/// Rejection still happens - one layer down, and with upstream's wording.
///
/// These two cases previously asserted a clap `ValueValidation` error, which
/// pinned the LAYER rather than the behaviour. The layer moved; the outcome
/// must not, so they now assert the outcome: a syntax-error exit with the
/// offending value named.
///
/// upstream: options.c:1253-1264 - `--block-size=abc is invalid`, exit 1.
#[test]
fn non_numeric_block_size_is_rejected_with_the_value_named() {
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(OC_RSYNC),
        OsString::from("--block-size=abc"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);
    let stderr = String::from_utf8_lossy(&stderr);

    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("--block-size") && stderr.contains("abc"),
        "the diagnostic must name the option and the value: {stderr}"
    );
}

/// upstream: options.c:1216-1221 - a negative size fails the `dsize < 0` check.
#[test]
fn negative_block_size_is_rejected_with_the_value_named() {
    let (code, _stdout, stderr) = run_with_args([
        OsString::from(OC_RSYNC),
        OsString::from("--block-size=-1"),
        OsString::from("src"),
        OsString::from("dst"),
    ]);
    let stderr = String::from_utf8_lossy(&stderr);

    assert_eq!(code, 1, "stderr: {stderr}");
    assert!(
        stderr.contains("--block-size") && stderr.contains("-1"),
        "the diagnostic must name the option and the value: {stderr}"
    );
}
