//! Value coercion and validation helpers for numeric/sized CLI options.
//!
//! These parse and range-check the integer and byte-sized arguments
//! (`--rayon-threads`, `--tokio-threads`, `--spill-threshold-bytes`) before
//! they reach the strongly-typed [`ParsedArgs`](super::ParsedArgs) struct.

use std::ffi::OsString;

/// Maximum thread count accepted by `--rayon-threads` / `--tokio-threads`.
///
/// Mirrors the sanity ceiling applied by the broader thread-tunable surface
/// (`crossbeam`/`tokio` historically reject very large pools at runtime).
const MAX_THREAD_COUNT: u32 = 1024;

/// Parses a thread-count CLI option (`--rayon-threads`, `--tokio-threads`).
///
/// Accepts a positive base-10 integer in the inclusive range `1..=1024`.
/// Returns `Ok(None)` when the option was not supplied, allowing callers
/// to keep the runtime's own default thread count.
pub(super) fn parse_thread_count(
    matches: &mut clap::ArgMatches,
    flag: &'static str,
) -> Result<Option<u32>, clap::Error> {
    let Some(value) = matches.remove_one::<OsString>(flag) else {
        return Ok(None);
    };
    let raw = value.to_string_lossy();
    match raw.parse::<u32>() {
        Ok(0) => Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!(
                "invalid --{flag} value '{raw}': must be a positive integer (1-{MAX_THREAD_COUNT})\n"
            ),
        )),
        Ok(n) if n > MAX_THREAD_COUNT => Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!("invalid --{flag} value '{raw}': must not exceed {MAX_THREAD_COUNT}\n"),
        )),
        Ok(n) => Ok(Some(n)),
        Err(_) => Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!(
                "invalid --{flag} value '{raw}': must be a positive integer (1-{MAX_THREAD_COUNT})\n"
            ),
        )),
    }
}

/// Parses the `--spill-threshold-bytes` value into a positive byte count.
///
/// Accepts a positive integer with an optional case-insensitive K/M/G/T/P/E
/// suffix interpreted as a power of 1024 (matching the
/// `OC_RSYNC_SPILL_THRESHOLD_BYTES` env-var grammar). `0` is rejected -
/// callers that want to disable spilling should omit the flag.
/// Returns `Ok(None)` when the flag was not supplied.
pub(super) fn parse_spill_threshold_bytes(
    matches: &mut clap::ArgMatches,
) -> Result<Option<u64>, clap::Error> {
    let Some(value) = matches.remove_one::<OsString>("spill-threshold-bytes") else {
        return Ok(None);
    };
    let raw = value.to_string_lossy();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            "invalid --spill-threshold-bytes value: must not be empty\n".to_string(),
        ));
    }
    let bytes = parse_spill_size(trimmed).ok_or_else(|| {
        clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!(
                "invalid --spill-threshold-bytes value '{raw}': must be a positive \
                 integer with an optional K/M/G/T/P/E suffix (base 1024)\n"
            ),
        )
    })?;
    if bytes == 0 {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!("invalid --spill-threshold-bytes value '{raw}': must be greater than zero\n"),
        ));
    }
    Ok(Some(bytes))
}

/// Parses a positive integer with an optional K/M/G/T/P/E suffix (base 1024).
///
/// Returns `None` when the input does not match the expected grammar or when
/// the multiplication overflows `u64`.
fn parse_spill_size(input: &str) -> Option<u64> {
    if input.is_empty() {
        return None;
    }
    let last = input.as_bytes().last().copied()?;
    let (digits, suffix) = if last.is_ascii_alphabetic() {
        (&input[..input.len() - 1], Some(last.to_ascii_uppercase()))
    } else {
        (input, None)
    };
    if digits.is_empty() {
        return None;
    }
    let base: u64 = digits.parse().ok()?;
    let multiplier: u64 = match suffix {
        None => 1,
        Some(b'K') => 1024,
        Some(b'M') => 1024u64.pow(2),
        Some(b'G') => 1024u64.pow(3),
        Some(b'T') => 1024u64.pow(4),
        Some(b'P') => 1024u64.pow(5),
        Some(b'E') => 1024u64.pow(6),
        _ => return None,
    };
    base.checked_mul(multiplier)
}
