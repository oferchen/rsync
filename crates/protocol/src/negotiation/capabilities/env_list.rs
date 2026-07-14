//! `RSYNC_CHECKSUM_LIST` / `RSYNC_COMPRESS_LIST` overrides for the algorithm
//! negotiation candidate lists.
//!
//! Upstream rsync lets an operator override or restrict the ordered list of
//! negotiable checksum and compression algorithms through two environment
//! variables. When set, the variable's whitespace-separated names replace the
//! built-in preference order a peer advertises during `negotiate_the_strings()`
//! and uses to select a mutually supported algorithm. An unset or empty value
//! leaves the built-in default order untouched, so the default wire bytes are
//! unchanged.
//!
//! Names are canonicalised to their wire spelling (e.g. the `xxhash` alias
//! becomes `xxh64`, matching upstream's `main_nni` rewrite), de-duplicated, and
//! kept in the order the variable lists them. Unrecognised or build-unsupported
//! names are dropped; when a value holds names but none survive, the parsed list
//! collapses to the literal `INVALID`, which fails negotiation just as upstream
//! does.
//!
//! # Upstream reference
//!
//! - `compat.c:409-424 getenv_nstr()` - reads the variable and applies the
//!   server-side `&` split.
//! - `compat.c:281-331 parse_nni_str()` - validates, de-duplicates and reorders
//!   the names, canonicalising aliases and emitting `INVALID` when no name
//!   survives.
//! - `compat.c:506-533 send_negotiate_str()` - advertises the parsed list,
//!   falling back to `get_default_nno_list()` when the value is empty.

use super::algorithms::{
    ChecksumAlgorithm, CompressionAlgorithm, SUPPORTED_CHECKSUMS, supported_compressions,
};

/// Environment variable that overrides the checksum negotiation list.
const CHECKSUM_LIST_ENV: &str = "RSYNC_CHECKSUM_LIST";
/// Environment variable that overrides the compression negotiation list.
const COMPRESS_LIST_ENV: &str = "RSYNC_COMPRESS_LIST";

/// Sentinel emitted when a value held names but none were recognised.
///
/// upstream: compat.c:327-328 `parse_nni_str()`.
const INVALID: &str = "INVALID";

/// An environment override applied to a negotiation candidate list.
pub(super) struct EnvOverride {
    /// Space-joined names to advertise on the wire. Equals [`INVALID`] when the
    /// value held names but none were recognised.
    pub advertised: String,
    /// Ordered canonical wire names used for local algorithm selection. Empty
    /// when `advertised` is [`INVALID`].
    pub candidates: Vec<&'static str>,
}

/// Returns the checksum candidate override from `RSYNC_CHECKSUM_LIST`, or
/// `None` when the variable is unset or holds only whitespace - in which case
/// the caller keeps the built-in default order.
pub(super) fn checksum_candidates(is_server: bool) -> Option<EnvOverride> {
    parse_env(CHECKSUM_LIST_ENV, is_server, resolve_checksum)
}

/// Returns the compression candidate override from `RSYNC_COMPRESS_LIST`, or
/// `None` when the variable is unset or holds only whitespace.
pub(super) fn compression_candidates(is_server: bool) -> Option<EnvOverride> {
    parse_env(COMPRESS_LIST_ENV, is_server, resolve_compression)
}

/// Resolves a checksum name to its canonical wire spelling, or `None` when the
/// name is not a build-supported algorithm. Accepts the `xxhash` alias and any
/// ASCII casing, mirroring upstream's case-insensitive `get_nni_by_name`.
fn resolve_checksum(name: &str) -> Option<&'static str> {
    let canonical = ChecksumAlgorithm::parse(&name.to_ascii_lowercase())
        .ok()?
        .as_str();
    SUPPORTED_CHECKSUMS
        .contains(&canonical)
        .then_some(canonical)
}

/// Resolves a compression name to its canonical wire spelling, or `None` when
/// the name is not a build-supported algorithm. Feature-gated codecs (lz4,
/// zstd) absent from this build are treated as unrecognised, matching upstream's
/// compile-time `valid_compressions_items[]` gating.
fn resolve_compression(name: &str) -> Option<&'static str> {
    let canonical = CompressionAlgorithm::parse(&name.to_ascii_lowercase())
        .ok()?
        .as_str();
    supported_compressions()
        .contains(&canonical)
        .then_some(canonical)
}

/// Core parser shared by both variables.
///
/// Mirrors `getenv_nstr()` + `parse_nni_str()`: applies the `&` client/server
/// split, tokenises on whitespace, resolves and de-duplicates names in the
/// listed order, drops unrecognised names, and yields the [`INVALID`] sentinel
/// when names were present but none survived.
fn parse_env(
    key: &str,
    is_server: bool,
    resolve: impl Fn(&str) -> Option<&'static str>,
) -> Option<EnvOverride> {
    let raw = std::env::var(key).ok()?;

    // upstream: compat.c:417-421 getenv_nstr - the server uses only the portion
    // after the first '&', while the client stops at it because parse_nni_str
    // treats '&' as a token terminator (compat.c:291-292). A value without '&'
    // is used whole by both sides.
    let scoped = match raw.split_once('&') {
        Some((before, after)) => {
            if is_server {
                after
            } else {
                before
            }
        }
        None => raw.as_str(),
    };

    // upstream: compat.c:435-438 / 512,519 - an empty or all-whitespace value is
    // treated as unset, leaving the built-in default order in place.
    scoped.split_whitespace().next()?;

    let mut candidates: Vec<&'static str> = Vec::new();
    for token in scoped.split_whitespace() {
        // upstream: compat.c:295-306 - unrecognised names are dropped and the
        // first occurrence of each algorithm wins (duplicates removed).
        if let Some(name) = resolve(token) {
            if !candidates.contains(&name) {
                candidates.push(name);
            }
        }
    }

    // upstream: compat.c:327-328 - names were seen but none were valid, so the
    // parsed list collapses to "INVALID", which is advertised verbatim and
    // subsequently fails to negotiate a mutual algorithm.
    if candidates.is_empty() {
        return Some(EnvOverride {
            advertised: INVALID.to_string(),
            candidates,
        });
    }

    Some(EnvOverride {
        advertised: candidates.join(" "),
        candidates,
    })
}
