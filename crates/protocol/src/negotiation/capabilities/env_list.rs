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

use super::algorithms::{resolve_checksum_name, resolve_compression_name};

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
///
/// `write_batch` replaces the variable outright with the single old-style
/// choice, so a `--write-batch` recording is decodable by a `--read-batch`
/// replay - which negotiates nothing and has only the batch header to go on.
/// upstream: `compat.c:412-414 getenv_nstr()`.
pub(super) fn checksum_candidates(
    is_server: bool,
    write_batch: bool,
    protocol: u8,
) -> Option<EnvOverride> {
    if write_batch {
        let forced = if protocol >= 30 { "md5" } else { "md4" };
        return parse_list(forced, resolve_checksum_name);
    }
    parse_env(CHECKSUM_LIST_ENV, is_server, resolve_checksum_name)
}

/// Returns the compression candidate override from `RSYNC_COMPRESS_LIST`, or
/// `None` when the variable is unset or holds only whitespace.
///
/// `write_batch` pins the list to `zlib` for the same reason
/// [`checksum_candidates`] pins the checksum.
/// upstream: `compat.c:412-414 getenv_nstr()`.
pub(super) fn compression_candidates(is_server: bool, write_batch: bool) -> Option<EnvOverride> {
    if write_batch {
        return parse_list("zlib", resolve_compression_name);
    }
    parse_env(COMPRESS_LIST_ENV, is_server, resolve_compression_name)
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
    validate_choice(CHECKSUM_LIST_ENV, "checksum", choice, resolve_checksum_name)
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
    validate_choice(
        COMPRESS_LIST_ENV,
        "compress",
        choice,
        resolve_compression_name,
    )
}

/// Refuses the forced fallback default checksum on the non-negotiated path when
/// `RSYNC_CHECKSUM_LIST` excludes it.
///
/// upstream: `compat.c:541-555 negotiate_the_strings` - when the peer is too old
/// to negotiate (`do_negotiated_strings == 0`) and no `--checksum-choice` is
/// forced, `send_negotiate_str` still parses the env list into `nno->saw`, then
/// `recv_negotiate_str` runs `parse_negotiate_str` with the prefilled default
/// (`"md5"` for protocol >= 30, else `"md4"`). When `saw` lacks the default the
/// parse fails and upstream aborts with `RERR_UNSUPPORTED` (`compat.c:406`).
/// Unlike [`validate_checksum_choice`] this runs on both sides, so pass the
/// caller's `is_server`; the `&` split then selects the caller's half.
pub(super) fn validate_default_checksum(default: &str, is_server: bool) -> io::Result<()> {
    validate_default(
        CHECKSUM_LIST_ENV,
        "checksum",
        default,
        is_server,
        resolve_checksum_name,
    )
}

/// Refuses the forced fallback default `zlib` compression on the non-negotiated
/// path when `RSYNC_COMPRESS_LIST` excludes it.
///
/// The compression counterpart of [`validate_default_checksum`], mirroring the
/// `valid_compressions.saw` / `"zlib"` prefill branch (`compat.c:557-564`). Only
/// reached when compression is active (`do_compression`), matching upstream's
/// gate on `send_negotiate_str` at `compat.c:544`.
pub(super) fn validate_default_compress(default: &str, is_server: bool) -> io::Result<()> {
    validate_default(
        COMPRESS_LIST_ENV,
        "compress",
        default,
        is_server,
        resolve_compression_name,
    )
}

/// Membership of a single algorithm in the env-parsed candidate list.
enum EnvMembership {
    /// The variable is unset or all-whitespace - no restriction applies.
    Unset,
    /// The variable restricts the list and the algorithm is a member.
    Present,
    /// The variable restricts the list and the algorithm is absent.
    Absent,
}

/// Parses the env list once and reports whether `algo` survives it.
///
/// Shared by the forced-choice refusal ([`validate_choice`]) and the
/// fallback-default refusal ([`validate_default`]) so both use the identical
/// `&`-split, tokenising, alias canonicalisation and de-duplication from
/// [`parse_env`] - the single source of truth for the candidate set. An empty
/// set (the `INVALID` sentinel) never contains `algo`, so a value whose names
/// were all unrecognised reports [`EnvMembership::Absent`], matching upstream.
fn env_membership(
    key: &str,
    is_server: bool,
    algo: &str,
    resolve: impl Fn(&str) -> Option<&'static str>,
) -> EnvMembership {
    match parse_env(key, is_server, resolve) {
        None => EnvMembership::Unset,
        Some(env) if env.candidates.contains(&algo) => EnvMembership::Present,
        Some(_) => EnvMembership::Absent,
    }
}

/// Shared refusal check for both forced-choice kinds.
///
/// Only the server validates a forced `--checksum-choice`/`--compress-choice`,
/// so it always parses with `is_server = true`. When the variable is unset or
/// empty the choice is accepted; otherwise the forced canonical name must be a
/// member of the parsed candidate set.
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
    // upstream: compat.c:432-445 - unset/blank accepts; saw[num] must be set.
    match env_membership(key, true, choice, resolve) {
        EnvMembership::Unset | EnvMembership::Present => Ok(()),
        // upstream: compat.c:446-448 rprintf(FERROR, "Your --%s-choice value
        // (%s) was refused by the server.\n", ...). The trailing newline is
        // added by the diagnostic layer, not embedded in the message.
        EnvMembership::Absent => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Your --{kind}-choice value ({choice}) was refused by the server."),
        )),
    }
}

/// Refuses the prefilled fallback default that upstream validates in
/// `recv_negotiate_str` when the env list excludes it.
///
/// A no-op when the variable is unset or the default is a member. On refusal,
/// emits upstream's full `recv_negotiate_str` block via
/// [`super::failure::negotiation_failure`] - the `Failed to negotiate a %s
/// choice.` line plus the offered Server/Client lists (`compat.c:381-405`) -
/// with [`io::ErrorKind::Unsupported`], which the core exit-code mapper turns
/// into `RERR_UNSUPPORTED` (exit 4), matching `exit_cleanup(RERR_UNSUPPORTED)`
/// at `compat.c:406`. The non-negotiated path prints the lists on both sides.
fn validate_default(
    key: &str,
    kind: &str,
    default: &str,
    is_server: bool,
    resolve: impl Fn(&str) -> Option<&'static str>,
) -> io::Result<()> {
    // upstream: compat.c:369-406 recv_negotiate_str - the non-negotiated path
    // prefills `tmpbuf` with the old-style default (compat.c:551-563) and, when
    // the env-parsed saw list rejects it, prints the offered lists on BOTH
    // sides (do_negotiated_strings == 0 makes `!am_server || ...` always true).
    match env_membership(key, is_server, default, &resolve) {
        EnvMembership::Unset | EnvMembership::Present => Ok(()),
        EnvMembership::Absent => {
            // Rebuild upstream's full block: the prefilled default IS its
            // `tmpbuf`, so it is the peer-list line; our env-parsed saw list is
            // the own-list line. Re-parse (cold error path) to recover it.
            let candidates = parse_env(key, is_server, resolve)
                .map(|env| env.candidates)
                .unwrap_or_default();
            Err(super::failure::negotiation_failure(
                kind,
                is_server,
                false,
                default,
                &candidates,
            ))
        }
    }
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

    parse_list(scoped, resolve)
}

/// Resolves one already-scoped whitespace-separated name list.
///
/// Split out of [`parse_env`] so the `write_batch` pin can feed a literal list
/// through the same `parse_nni_str()` semantics upstream applies to the
/// environment value.
fn parse_list(scoped: &str, resolve: impl Fn(&str) -> Option<&'static str>) -> Option<EnvOverride> {
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
