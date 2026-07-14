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
//! A recognised alias is rewritten to its canonical wire spelling (e.g. the
//! `xxhash` alias becomes `xxh64`, matching upstream's `main_nni` rewrite);
//! every other name keeps the operator's original bytes verbatim, including its
//! casing, so the advertised vstring is byte-for-byte what upstream emits.
//! Lookup and selection remain case-insensitive. Names are de-duplicated and
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

use std::io;

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

/// Refuses a client-forced `--checksum-choice` whose algorithm is absent from
/// the server's `RSYNC_CHECKSUM_LIST`.
///
/// Only the server validates, and only when the client explicitly forced the
/// choice - the caller gates on `is_server` and `checksum_override.is_some()`,
/// mirroring `checksum.c:185-186 parse_checksum_choice`
/// (`if (am_server && checksum_choice) validate_choice_vs_env(...)`). When the
/// variable is unset or holds only whitespace this is a no-op and any choice is
/// accepted, so the default (unset-env) path is unchanged.
///
/// # MD4 family
///
/// Upstream keeps four distinct MD4 name-num slots (`CSUM_MD4`,
/// `CSUM_MD4_OLD`, `CSUM_MD4_BUSTED`, `CSUM_MD4_ARCHAIC`) and, when `md4` is in
/// the env list, marks all four as seen (`compat.c:443-444`). oc-rsync collapses
/// the whole MD4 family into a single [`ChecksumAlgorithm::MD4`] whose wire name
/// is `md4`, so a forced MD4 choice matches iff `md4` is a candidate - the
/// special case is subsumed by the collapsed representation.
///
/// # Upstream reference
///
/// - `compat.c:426-449 validate_choice_vs_env()` - the refusal check itself.
/// - `checksum.c:185-186` - the server-only call site.
pub(super) fn validate_checksum_choice(choice: &str) -> io::Result<()> {
    validate_choice(CHECKSUM_LIST_ENV, "checksum", choice, resolve_checksum)
}

/// Refuses a client-forced `--compress-choice` whose algorithm is absent from
/// the server's `RSYNC_COMPRESS_LIST`.
///
/// The compression counterpart of [`validate_checksum_choice`], mirroring
/// `compat.c:193-194 parse_compress_choice`
/// (`if (am_server) validate_choice_vs_env(NSTR_COMPRESS, do_compression, -1)`).
///
/// # Upstream reference
///
/// - `compat.c:426-449 validate_choice_vs_env()`.
/// - `compat.c:193-194` - the server-only call site.
pub(super) fn validate_compress_choice(choice: &str) -> io::Result<()> {
    validate_choice(COMPRESS_LIST_ENV, "compress", choice, resolve_compression)
}

/// Shared refusal check for both choice kinds.
///
/// Reuses [`parse_env`] with `is_server = true` (only the server validates) so
/// the env list is parsed exactly once, with the same `&` split, tokenising,
/// alias canonicalisation and de-duplication used to build the advertised list -
/// no separate parse. When the variable is unset or empty, [`parse_env`] returns
/// `None` and the choice is accepted. Otherwise the forced canonical name must
/// appear in the parsed candidate set (an empty set - the `INVALID` sentinel -
/// never contains it, so a value whose names were all unrecognised refuses every
/// choice, matching upstream).
///
/// On refusal, emits the byte-exact upstream message and fails with an
/// [`io::ErrorKind::Unsupported`] error, which the core exit-code mapper turns
/// into `RERR_UNSUPPORTED` (exit 4) - the code `validate_choice_vs_env` passes to
/// `exit_cleanup` (`compat.c:449`).
fn validate_choice(
    key: &str,
    kind: &str,
    choice: &str,
    resolve: impl Fn(&str) -> Option<&'static str>,
) -> io::Result<()> {
    // upstream: compat.c:432-433 - an unset or all-whitespace list_str returns
    // early, leaving the choice unvalidated (accepted).
    let Some(env) = parse_env(key, true, resolve) else {
        return Ok(());
    };

    // upstream: compat.c:445 - saw[num] must be set for the forced choice(s).
    if env.candidates.contains(&choice) {
        return Ok(());
    }

    // upstream: compat.c:446-448 rprintf(FERROR, "Your --%s-choice value (%s)
    // was refused by the server.\n", ...). The trailing newline is added by the
    // diagnostic layer, not embedded in the message, matching oc convention.
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("Your --{kind}-choice value ({choice}) was refused by the server."),
    ))
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
    let mut advertised: Vec<String> = Vec::new();
    for token in scoped.split_whitespace() {
        // upstream: compat.c:295-306 - unrecognised names are dropped and the
        // first occurrence of each algorithm wins (duplicates removed).
        if let Some(canonical) = resolve(token) {
            if !candidates.contains(&canonical) {
                candidates.push(canonical);
                // upstream: compat.c:298-304 - only a recognised alias (an entry
                // whose main_nni points elsewhere) is rewritten to its canonical
                // spelling; every other name keeps the operator's original bytes
                // verbatim on the wire, including casing. A token that differs
                // from its canonical name only in ASCII case is not an alias.
                if token.eq_ignore_ascii_case(canonical) {
                    advertised.push(token.to_string());
                } else {
                    advertised.push(canonical.to_string());
                }
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
        advertised: advertised.join(" "),
        candidates,
    })
}
