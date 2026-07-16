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
//! - `options.c:1954-1965` - `max_alloc` is rewritten from `--max-alloc` /
//!   `RSYNC_MAX_ALLOC` during option processing.
//! - `util2.c:73-81` - `my_alloc()` aborts with `RERR_MALLOC` once a request
//!   reaches `max_alloc`.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Default `--max-alloc` ceiling in bytes (1 GiB).
///
/// upstream: `options.c:203` `#define DEFAULT_MAX_ALLOC (1024L * 1024 * 1024)`.
pub const DEFAULT_MAX_ALLOC: usize = 1024 * 1024 * 1024;

/// The negotiated `--max-alloc` ceiling, mirroring upstream's `max_alloc`
/// global (`options.c:204`). Defaults to [`DEFAULT_MAX_ALLOC`] until
/// [`set_max_alloc`] runs during option processing.
static MAX_ALLOC: AtomicUsize = AtomicUsize::new(DEFAULT_MAX_ALLOC);

/// Sets the process-wide `--max-alloc` ceiling in bytes.
///
/// Called once during option processing by whichever side owns the receive
/// path (the client's `apply_max_alloc`, or the server's `--max-alloc`
/// handling). A zero value is ignored, leaving the previous ceiling in place,
/// mirroring upstream's rejection of a non-positive size in `parse_size_arg`.
///
/// upstream: `options.c:1959-1965` assigns `max_alloc = size` after parsing.
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
