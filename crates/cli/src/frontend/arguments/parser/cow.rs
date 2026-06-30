//! Copy-on-write policy resolution for `--reflink` / `--cow` / `--no-cow`.
//!
//! Resolves the final [`fast_io::CowPolicy`] from the binary `--cow`/`--no-cow`
//! flags and the optional tri-state `--reflink` value, honouring upstream
//! rsync's left-to-right "last flag wins" precedence.

/// Parses a `--reflink=<MODE>` value into a [`fast_io::CowPolicy`].
///
/// Accepted values mirror the upstream-style tri-state convention used by
/// `git`, `cp(1)`, and other reflink-aware tools:
///
/// - `auto` -> [`fast_io::CowPolicy::Auto`]
/// - `always` -> [`fast_io::CowPolicy::Required`]
/// - `never` -> [`fast_io::CowPolicy::Disabled`]
///
/// Returns `None` for any other input so the caller can surface a
/// `clap::Error` with the canonical "expected one of ..." message.
pub(super) fn parse_reflink_mode(input: &str) -> Option<fast_io::CowPolicy> {
    match input {
        "auto" => Some(fast_io::CowPolicy::Auto),
        "always" => Some(fast_io::CowPolicy::Required),
        "never" => Some(fast_io::CowPolicy::Disabled),
        _ => None,
    }
}

/// Resolves the final [`fast_io::CowPolicy`] from the binary
/// `--cow`/`--no-cow` flags and the optional tri-state `--reflink` value.
///
/// The flag that appears last on the command line wins, matching upstream
/// rsync's left-to-right option processing. When neither `--cow` nor
/// `--no-cow` is present the `--reflink` value is used directly; when
/// no form is present the default is [`fast_io::CowPolicy::Auto`].
///
/// `--cow` and `--no-cow` share a `clap::overrides_with` pair so only the
/// later of the two is "present" in matches; we use its index to compare
/// against the `--reflink` index.
pub(super) fn resolve_cow_policy(
    matches: &clap::ArgMatches,
    reflink_explicit: Option<fast_io::CowPolicy>,
    reflink_index: Option<usize>,
) -> fast_io::CowPolicy {
    let (binary_policy, binary_index) = if matches.get_flag("no-cow") {
        (
            Some(fast_io::CowPolicy::Disabled),
            last_occurrence(matches, "no-cow"),
        )
    } else if matches.get_flag("cow") {
        (
            Some(fast_io::CowPolicy::Auto),
            last_occurrence(matches, "cow"),
        )
    } else {
        (None, None)
    };

    match (reflink_explicit, binary_policy) {
        (None, None) => fast_io::CowPolicy::Auto,
        (Some(reflink), None) => reflink,
        (None, Some(binary)) => binary,
        (Some(reflink), Some(binary)) => match (reflink_index, binary_index) {
            (Some(r), Some(b)) if r >= b => reflink,
            (Some(_), Some(_)) => binary,
            (Some(_), None) => reflink,
            (None, Some(_)) => binary,
            (None, None) => reflink,
        },
    }
}

/// Returns the highest argument index for a given flag id, mirroring the
/// helper in [`super::flags`]. Inlined here to avoid widening the visibility
/// of the existing private helper.
pub(super) fn last_occurrence(matches: &clap::ArgMatches, id: &str) -> Option<usize> {
    matches.indices_of(id).and_then(Iterator::max)
}
