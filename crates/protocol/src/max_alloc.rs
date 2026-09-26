//! Process-global `--max-alloc` allocation ceiling.
//!
//! Upstream rsync bounds every attacker-controlled wire allocation by a single
//! `size_t max_alloc` global rather than a per-field constant. It is seeded to
//! [`DEFAULT_MAX_ALLOC`] (1 GiB) and rewritten once during option processing
//! when `--max-alloc` (or `RSYNC_MAX_ALLOC`) is supplied; `my_alloc()` then
//! rejects any request that would meet or exceed it. Wire decoders that
//! allocate a peer-declared length consult this value so that a peer which
//! raised `--max-alloc` may legitimately send larger data, up to the field's
//! own signed-`int32` encoding ceiling (`0x7fffffff`).
//!
//! # Upstream Reference
//!
//! - `options.c:203-204` - `#define DEFAULT_MAX_ALLOC (1024L * 1024 * 1024)` and
//!   `size_t max_alloc = DEFAULT_MAX_ALLOC;`.
//! - `options.c:1960-1971` - `max_alloc` is rewritten from `--max-alloc` /
//!   `RSYNC_MAX_ALLOC` during option processing.
//! - `util2.c:73-81` - `my_alloc()` aborts with `RERR_MALLOC` once a request
//!   reaches `max_alloc`.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Default `--max-alloc` ceiling in bytes (1 GiB).
///
/// upstream: `options.c:209` `#define DEFAULT_MAX_ALLOC (1024L * 1024 * 1024)`.
pub const DEFAULT_MAX_ALLOC: usize = 1024 * 1024 * 1024;

/// The negotiated `--max-alloc` ceiling, mirroring upstream's `max_alloc`
/// global (`options.c:204`). Defaults to [`DEFAULT_MAX_ALLOC`] until
/// [`set_max_alloc`] runs during option processing.
static MAX_ALLOC: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_ALLOC);

/// Smallest `--max-alloc` upstream's `parse_size_arg` accepts.
///
/// upstream: `options.c:2073` passes `min_value = 1024*1024` to
/// `parse_size_arg(max_alloc_arg, 'B', "max-alloc", 1024*1024, -1, True)`.
pub const MIN_MAX_ALLOC: u64 = 1024 * 1024;

/// The ceiling `--max-alloc` resolves to and must stay below.
///
/// `SIZE_MAX / 2` rather than `SIZE_MAX`, because upstream's parser returns
/// the size as a signed `ssize_t`. It is also what `--max-alloc=0` means: the
/// largest limit this build supports, never an unbounded one.
///
/// upstream: `options.c:1164` `#define SIZE_ARG_MAX ((ssize_t)(SIZE_MAX / 2))`.
pub const SIZE_ARG_MAX: u64 = (usize::MAX / 2) as u64;

/// Applies upstream's `--max-alloc` value rules to an already-parsed byte
/// count, returning the resolved limit or the message upstream emits.
///
/// This is the ONE owner of the rules. Both decode paths reach a peer-supplied
/// `--max-alloc`: the client's own CLI, and the `--server` / daemon argv
/// parsers that read what a peer forwarded. Upstream runs the identical block
/// in `parse_arguments()` regardless of which end is executing it
/// (`options.c:2067-2086`), so the rules must not be restated per caller -
/// a caller that took a forwarded `0` literally instead of resolving it would
/// hand a peer the ability to disable the `my_alloc()` ceiling.
///
/// `display` is the operator's spelling of the value, used only to render the
/// two size diagnostics the way upstream's `parse_size_arg` does.
///
/// # Errors
///
/// Returns upstream's diagnostic text when the value is non-zero and below
/// [`MIN_MAX_ALLOC`], or not below [`SIZE_ARG_MAX`].
pub fn validate_max_alloc(limit: u64, display: &str) -> Result<u64, String> {
    // upstream: options.c:2074-2086 - `unlimited_0` lets parse_size_arg return
    // 0, and 0 then resolves to SIZE_ARG_MAX: the same ceiling an explicit
    // value is held below, so the guard stays bounded. A peer-forwarded 0 is
    // resolved the same way, against this side's own SIZE_MAX.
    if limit == 0 {
        return Ok(SIZE_ARG_MAX);
    }

    // upstream: options.c:1250-1255 - a value below the 1 MiB minimum is "too
    // small"; do_big_num renders the constant as "1.00M", and `unlimited_0`
    // adds the "or 0 for unlimited" clause.
    if limit < MIN_MAX_ALLOC {
        return Err(format!(
            "--max-alloc={display} is too small (min: 1.00M or 0 for unlimited)"
        ));
    }

    // upstream: options.c:1221-1227 - with no explicit maximum, a value is
    // "too large" once it reaches SIZE_ARG_MAX, compared as a double.
    if limit as f64 >= SIZE_ARG_MAX as f64 {
        return Err(format!("--max-alloc={display} is too large"));
    }

    Ok(limit)
}

/// Sets the process-wide `--max-alloc` ceiling in bytes.
///
/// Called once during option processing by whichever side owns the receive
/// path (the client's `apply_max_alloc`, or the server's `--max-alloc`
/// handling). [`validate_max_alloc`] never yields zero, so a zero here can only
/// come from a library caller; it is ignored, leaving the previous ceiling in
/// place rather than lifting it.
///
/// upstream: `options.c:2076-2089` assigns `max_alloc = size` after parsing.
pub fn set_max_alloc(bytes: usize) {
    if bytes == 0 {
        return;
    }
    MAX_ALLOC.store(bytes, Ordering::Relaxed);
}

/// Returns the effective `--max-alloc` ceiling in bytes.
///
/// Wire decoders compare a peer-declared allocation length against this value.
/// Defaults to [`DEFAULT_MAX_ALLOC`] (1 GiB) until [`set_max_alloc`] runs.
///
/// upstream: `util2.c:75` reads the `max_alloc` global inside `my_alloc()`.
#[must_use]
pub fn effective_max_alloc() -> usize {
    MAX_ALLOC.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_MAX_ALLOC, effective_max_alloc, set_max_alloc};

    /// The ceiling defaults to the upstream `DEFAULT_MAX_ALLOC` so that, absent
    /// an explicit `--max-alloc`, decoders enforce the same 1 GiB bound upstream
    /// applies by default. upstream: options.c:204.
    #[test]
    fn defaults_to_upstream_default() {
        assert_eq!(effective_max_alloc(), DEFAULT_MAX_ALLOC);
    }

    /// A raised ceiling is observed by later reads, matching upstream where the
    /// rewritten `max_alloc` global governs every subsequent allocation guard.
    #[test]
    fn set_then_read_roundtrips() {
        let restore = effective_max_alloc();
        set_max_alloc(2 * DEFAULT_MAX_ALLOC);
        assert_eq!(effective_max_alloc(), 2 * DEFAULT_MAX_ALLOC);
        set_max_alloc(restore);
    }

    /// A zero size is ignored, mirroring upstream's rejection of a non-positive
    /// `--max-alloc`; the prior ceiling stays in force.
    #[test]
    fn zero_is_ignored() {
        let restore = effective_max_alloc();
        set_max_alloc(4096);
        set_max_alloc(0);
        assert_eq!(effective_max_alloc(), 4096);
        set_max_alloc(restore);
    }
}
