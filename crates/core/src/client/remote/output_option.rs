//! Server-side `--info=` argument construction.
//!
//! Mirrors upstream `make_output_option()` (options.c:360) as
//! `server_options()` calls it: for `info_words` only (options.c:3127-3129).
//! Upstream never sends `--debug=` to the server; a server's debug levels come
//! from the forwarded `-v` letters or from an explicit `-M--debug=X`.
//! Only levels the user set EXPLICITLY via `--info=` are forwarded: levels
//! merely implied by `-v`/`--stats` carry `DEFAULT_PRIORITY`, which upstream
//! skips (options.c:378), so the peer re-derives them from the forwarded `-v`
//! letters. Each word is additionally gated by its role `where` mask
//! (options.c:376) so a category is only sent when it is relevant to the
//! peer's role.

use std::ffi::OsString;

// upstream: options.c:270-273 - output-word role bit masks.
const W_CLI: u8 = 1 << 0;
const W_SRV: u8 = 1 << 1;
const W_SND: u8 = 1 << 2;
const W_REC: u8 = 1 << 3;

// upstream: options.c:286-301 info_words[] (rsync-3.5.1) - `(name, where)` in
// upstream table order. `stats` is deliberately not forwarded from here: oc conflates
// `--stats` with `--info=stats` into a single stats level and forwards it via
// the standalone `--stats` flag (mirroring upstream `if (do_stats) --stats` at
// options.c:3032), so the caller filters `stats` out of the enabled list to
// avoid double-sending.
const INFO_WORDS: &[(&str, u8)] = &[
    ("backup", W_REC),
    ("copy", W_REC),
    ("del", W_REC),
    ("flist", W_CLI),
    ("misc", W_SND | W_REC),
    ("mount", W_SND | W_REC),
    ("name", W_SND | W_REC),
    ("nonreg", W_REC),
    ("progress", W_CLI),
    ("remove", W_SND),
    ("skip", W_REC),
    ("stats", W_CLI | W_SRV),
    ("symsafe", W_SND | W_REC),
];

/// Splits a normalized `name{level}` token into its name and level.
///
/// A token with no trailing digits (e.g. `del`) is level 1, matching upstream's
/// convention where level 1 is implied by the bare name (options.c:432).
fn split_name_level(token: &str) -> (&str, u8) {
    let name = token.trim_end_matches(|c: char| c.is_ascii_digit());
    if name.len() == token.len() {
        (name, 1)
    } else {
        let level = token[name.len()..].parse::<u8>().unwrap_or(1);
        (name, level)
    }
}

/// Builds the `--info=` server argument, or `None` when no enabled
/// flag is relevant to the peer's role.
///
/// `enabled` holds the explicitly-set flags as normalized `name{level}` tokens
/// (e.g. `io2`, `del`); `am_sender` is true on a push (the local side is the
/// sender). Mirrors upstream `make_output_option()` (options.c:360): each word
/// is gated by its role `where` mask against `W_SRV | (am_sender ? W_REC :
/// W_SND)`, and a level of 1 is emitted bare because the digit is implied
/// (options.c:432). The abbreviated `ALL`/`NONE`/`ALLn` spelling upstream may
/// use is a pure length optimization that parses to the identical peer state,
/// so the expanded comma list is emitted verbatim.
pub(crate) fn make_output_option(enabled: &[OsString], am_sender: bool) -> Option<String> {
    // upstream: options.c:3127 - `where = (am_server ? W_CLI : W_SRV) |
    // (am_sender ? W_REC : W_SND)`. We always build the peer's server args, so
    // `am_server` is false and the client-half is `W_SRV`.
    let peer_where = W_SRV | if am_sender { W_REC } else { W_SND };

    let mut tokens: Vec<String> = Vec::new();
    for raw in enabled {
        let token = raw.to_string_lossy();
        let (name, level) = split_name_level(&token);
        let forward = INFO_WORDS
            .iter()
            .any(|(n, mask)| *n == name && mask & peer_where != 0);
        if !forward {
            continue;
        }
        if level == 1 {
            tokens.push(name.to_owned());
        } else {
            tokens.push(format!("{name}{level}"));
        }
    }

    if tokens.is_empty() {
        None
    } else {
        Some(format!("--info={}", tokens.join(",")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    // WHY: this role-mask table is a deliberate copy of upstream's word names
    // (the masks are the payload the parser tables do not carry); pin the
    // names to upstream rsync 3.5.1 so a word added or renamed in one place
    // fails here instead of silently forwarding or dropping a category.
    #[test]
    fn word_table_names_exactly_the_upstream_info_words() {
        // upstream: options.c:286-301 info_words[] (rsync-3.5.1), table order.
        const UPSTREAM_INFO: [&str; 13] = [
            "backup", "copy", "del", "flist", "misc", "mount", "name", "nonreg", "progress",
            "remove", "skip", "stats", "symsafe",
        ];
        let info_names: Vec<&str> = INFO_WORDS.iter().map(|(name, _)| *name).collect();
        assert_eq!(info_names, UPSTREAM_INFO);
    }

    // WHY: a level-1 flag must be emitted bare so the peer parses it to the
    // identical level; a digit would over-specify. Mirrors options.c:432.
    #[test]
    fn level_one_emitted_without_digit() {
        let arg = make_output_option(&os(&["del1"]), true);
        assert_eq!(arg.as_deref(), Some("--info=del"));
    }

    // WHY: non-default levels must carry the digit so the peer raises its
    // output level to match the user's request.
    #[test]
    fn higher_level_keeps_digit() {
        let arg = make_output_option(&os(&["del2"]), true);
        assert_eq!(arg.as_deref(), Some("--info=del2"));
    }

    // WHY: a category the peer's role cannot act on must not be forwarded, or
    // the two sides disagree on which side owns the message. `del` is
    // receiver-side (W_REC); on a pull the peer is the sender (W_SND), so it is
    // dropped. Mirrors the `where` gate at options.c:376.
    #[test]
    fn role_filter_drops_irrelevant_category_on_pull() {
        // Pull: local is receiver, am_sender = false, peer_where = W_SRV|W_SND.
        let arg = make_output_option(&os(&["del1"]), false);
        assert_eq!(arg, None);
    }

    // WHY: the same receiver-side category IS relevant when the peer is the
    // receiver (push), so it must reach the peer.
    #[test]
    fn role_filter_keeps_receiver_category_on_push() {
        let arg = make_output_option(&os(&["del1"]), true);
        assert_eq!(arg.as_deref(), Some("--info=del"));
    }

    // WHY: client-only categories (progress/flist for info) are never the
    // peer's concern and must never be forwarded in either direction.
    #[test]
    fn client_only_categories_never_forwarded() {
        assert_eq!(make_output_option(&os(&["progress2"]), true), None);
        assert_eq!(make_output_option(&os(&["flist2"]), false), None);
    }

    // WHY: only words in upstream's info table are forwarded; anything else
    // (such as a debug word) never reaches the peer through this builder.
    #[test]
    fn a_word_outside_the_info_table_is_not_forwarded() {
        assert_eq!(make_output_option(&os(&["iouring1"]), true), None);
    }

    // WHY: no enabled flag means no argument at all - an empty `--info=` would
    // wrongly reset the peer's levels.
    #[test]
    fn empty_enabled_yields_none() {
        assert_eq!(make_output_option(&[], true), None);
    }

    // WHY: multiple forwarded categories join in table order with commas, as
    // upstream builds a single comma-separated value.
    #[test]
    fn multiple_categories_joined() {
        let arg = make_output_option(&os(&["del1", "remove1", "skip2"]), true);
        // Push peer_where = W_SRV|W_REC: del and skip are W_REC (kept), remove
        // is W_SND (dropped).
        assert_eq!(arg.as_deref(), Some("--info=del,skip2"));

        let pull = make_output_option(&os(&["del1", "remove1", "skip2"]), false);
        // Pull peer_where = W_SRV|W_SND: remove kept, del and skip dropped.
        assert_eq!(pull.as_deref(), Some("--info=remove"));
    }
}
