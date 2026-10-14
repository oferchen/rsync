//! Value coercion and validation helpers for numeric/sized CLI options.
//!
//! These parse and range-check the integer and byte-sized arguments
//! (`--rayon-threads`, `--tokio-threads`, `--checksum-threads`) before
//! they reach the strongly-typed [`ParsedArgs`](super::ParsedArgs) struct.
//!
//! Deviation from upstream: every option parsed here is an oc-only local
//! tuning knob with no upstream rsync 3.5.1 counterpart.

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

/// Resolved `--checksum-threads` request.
///
/// Local-only performance knob controlling parallel basis-signature hashing.
/// Never forwarded to the remote server and never changes wire bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumThreadsSetting {
    /// `auto` / `0`: parallel above the size threshold using available cores.
    Auto,
    /// `1`: force sequential hashing.
    Sequential,
    /// `N > 1`: parallel, capping the rayon pool to `N` threads.
    Capped(u32),
}

/// Parses the `--checksum-threads` value.
///
/// Accepts `auto` (case-insensitive), `0` (both meaning parallel-auto), `1`
/// (force sequential), or `N` in `2..=1024` (parallel, cap the rayon pool to
/// `N`). Returns `Ok(None)` when the flag was not supplied, leaving the
/// bench-validated default (parallel-auto) in place.
pub(super) fn parse_checksum_threads(
    matches: &mut clap::ArgMatches,
) -> Result<Option<ChecksumThreadsSetting>, clap::Error> {
    let Some(value) = matches.remove_one::<OsString>("checksum-threads") else {
        return Ok(None);
    };
    let raw = value.to_string_lossy();
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("auto") {
        return Ok(Some(ChecksumThreadsSetting::Auto));
    }
    let invalid = || {
        clap::Error::raw(
            clap::error::ErrorKind::ValueValidation,
            format!(
                "invalid --checksum-threads value '{raw}': must be 'auto', 0 \
                 (parallel), 1 (sequential), or 2-{MAX_THREAD_COUNT} (cap)\n"
            ),
        )
    };
    match trimmed.parse::<u32>() {
        Ok(0) => Ok(Some(ChecksumThreadsSetting::Auto)),
        Ok(1) => Ok(Some(ChecksumThreadsSetting::Sequential)),
        Ok(n) if n <= MAX_THREAD_COUNT => Ok(Some(ChecksumThreadsSetting::Capped(n))),
        _ => Err(invalid()),
    }
}
