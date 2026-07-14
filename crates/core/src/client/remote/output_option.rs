//! Server-side `--info=`/`--debug=` argument construction.
//!
//! Mirrors upstream `options.c:make_output_option()` (rsync-3.4.4:354), which
//! forwards the negotiated info/debug output levels to the remote peer so its
//! diagnostic output matches what the user asked for. Only levels the user set
//! EXPLICITLY via `--info=`/`--debug=` are forwarded: levels merely implied by
//! `-v`/`--stats` carry `DEFAULT_PRIORITY`, which upstream skips
//! (options.c:372), so the peer re-derives them from the forwarded `-v`
//! letters. Each word is additionally gated by its role `where` mask
//! (options.c:419) so a category is only sent when it is relevant to the
//! peer's role.

use std::ffi::OsString;

// upstream: options.c:264-267 - output-word role bit masks.
const W_CLI: u8 = 1 << 0;
const W_SRV: u8 = 1 << 1;
const W_SND: u8 = 1 << 2;
const W_REC: u8 = 1 << 3;

/// Selects the info or debug output-word table.
#[derive(Clone, Copy)]
pub(crate) enum OutputWordKind {
    /// `--info=` categories (upstream `info_words[]`).
    Info,
    /// `--debug=` categories (upstream `debug_words[]`).
    Debug,
}

// upstream: options.c:280-294 info_words[] - `(name, where)` in upstream table
// order. `stats` is deliberately not forwarded from here: oc conflates
// `--stats` with `--info=stats` into a single stats level and forwards it via
// the standalone `--stats` flag (mirroring upstream `if (do_stats) --stats` at
// options.c:2856), so the caller filters `stats` out of the enabled list to
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
    ("stats", W_CLI | W_SRV),
    ("skip", W_REC),
    ("symsafe", W_SND | W_REC),
];

// upstream: options.c:297-322 debug_words[] - `(name, where)` in upstream
// table order. Categories oc adds beyond upstream (the accelerated-I/O
// diagnostics `iouring`/`clone`/`sockopt`/`iocp`) are absent here and fall to
// the unconditional-forward path in `make_output_option`.
const DEBUG_WORDS: &[(&str, u8)] = &[
    ("acl", W_SND | W_REC),
    ("backup", W_REC),
    ("bind", W_CLI),
    ("chdir", W_CLI | W_SRV),
    ("connect", W_CLI),
    ("cmd", W_CLI),
    ("del", W_REC),
    ("deltasum", W_SND | W_REC),
    ("dup", W_REC),
    ("exit", W_CLI | W_SRV),
    ("filter", W_SND | W_REC),
    ("flist", W_SND | W_REC),
    ("fuzzy", W_REC),
    ("genr", W_REC),
    ("hash", W_SND | W_REC),
    ("hlink", W_SND | W_REC),
    ("iconv", W_CLI | W_SRV),
    ("io", W_CLI | W_SRV),
    ("nstr", W_CLI | W_SRV),
    ("own", W_REC),
    ("proto", W_CLI | W_SRV),
    ("recv", W_REC),
    ("send", W_SND),
    ("time", W_REC),
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

/// Builds the `--info=`/`--debug=` server argument, or `None` when no enabled
/// flag is relevant to the peer's role.
///
/// `enabled` holds the explicitly-set flags as normalized `name{level}` tokens
/// (e.g. `io2`, `del`); `am_sender` is true on a push (the local side is the
/// sender). Mirrors upstream `make_output_option()` (options.c:354): each word
/// is gated by its role `where` mask against `W_SRV | (am_sender ? W_REC :
/// W_SND)`, and a level of 1 is emitted bare because the digit is implied
/// (options.c:432). The abbreviated `ALL`/`NONE`/`ALLn` spelling upstream may
/// use is a pure length optimization that parses to the identical peer state,
/// so the expanded comma list is emitted verbatim.
pub(crate) fn make_output_option(
    kind: OutputWordKind,
    enabled: &[OsString],
    am_sender: bool,
) -> Option<String> {
    let (table, prefix) = match kind {
        OutputWordKind::Info => (INFO_WORDS, "--info="),
        OutputWordKind::Debug => (DEBUG_WORDS, "--debug="),
    };
    // upstream: options.c:2946 - `where = (am_server ? W_CLI : W_SRV) |
    // (am_sender ? W_REC : W_SND)`. We always build the peer's server args, so
    // `am_server` is false and the client-half is `W_SRV`.
    let peer_where = W_SRV | if am_sender { W_REC } else { W_SND };

    let mut tokens: Vec<String> = Vec::new();
    for raw in enabled {
        let token = raw.to_string_lossy();
        let (name, level) = split_name_level(&token);
        let forward = match table.iter().find(|(n, _)| *n == name) {
            Some((_, mask)) => mask & peer_where != 0,
            // oc-specific categories absent from upstream's table (the
            // accelerated-I/O diagnostics) are forwarded unconditionally: an
            // oc peer consumes them and an upstream peer silently ignores
            // unknown tokens (options.c:465 am_server tolerance).
            None => true,
        };
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
        Some(format!("{prefix}{}", tokens.join(",")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    // WHY: a level-1 flag must be emitted bare so the peer parses it to the
    // identical level; a digit would over-specify. Mirrors options.c:432.
    #[test]
    fn level_one_emitted_without_digit() {
        let arg = make_output_option(OutputWordKind::Info, &os(&["del1"]), true);
        assert_eq!(arg.as_deref(), Some("--info=del"));
    }

    // WHY: non-default levels must carry the digit so the peer raises its
    // output level to match the user's request.
    #[test]
    fn higher_level_keeps_digit() {
        let arg = make_output_option(OutputWordKind::Debug, &os(&["io2"]), false);
        assert_eq!(arg.as_deref(), Some("--debug=io2"));
    }

    // WHY: a category the peer's role cannot act on must not be forwarded, or
    // the two sides disagree on which side owns the message. `del` is
    // receiver-side (W_REC); on a pull the peer is the sender (W_SND), so it is
    // dropped. Mirrors the `where` gate at options.c:419.
    #[test]
    fn role_filter_drops_irrelevant_category_on_pull() {
        // Pull: local is receiver, am_sender = false, peer_where = W_SRV|W_SND.
        let arg = make_output_option(OutputWordKind::Info, &os(&["del1"]), false);
        assert_eq!(arg, None);
    }

    // WHY: the same receiver-side category IS relevant when the peer is the
    // receiver (push), so it must reach the peer.
    #[test]
    fn role_filter_keeps_receiver_category_on_push() {
        let arg = make_output_option(OutputWordKind::Info, &os(&["del1"]), true);
        assert_eq!(arg.as_deref(), Some("--info=del"));
    }

    // WHY: client-only categories (progress/flist for info) are never the
    // peer's concern and must never be forwarded in either direction.
    #[test]
    fn client_only_categories_never_forwarded() {
        assert_eq!(
            make_output_option(OutputWordKind::Info, &os(&["progress2"]), true),
            None
        );
        assert_eq!(
            make_output_option(OutputWordKind::Info, &os(&["flist2"]), false),
            None
        );
    }

    // WHY: oc-specific debug categories have no upstream word; they must still
    // reach an oc peer (and are tolerated by an upstream peer).
    #[test]
    fn oc_specific_debug_category_forwarded() {
        let arg = make_output_option(OutputWordKind::Debug, &os(&["iouring1"]), true);
        assert_eq!(arg.as_deref(), Some("--debug=iouring"));
    }

    // WHY: no enabled flag means no argument at all - an empty `--info=` would
    // wrongly reset the peer's levels.
    #[test]
    fn empty_enabled_yields_none() {
        assert_eq!(make_output_option(OutputWordKind::Info, &[], true), None);
    }

    // WHY: multiple forwarded categories join in table order with commas, as
    // upstream builds a single comma-separated value.
    #[test]
    fn multiple_categories_joined() {
        let arg = make_output_option(OutputWordKind::Debug, &os(&["recv1", "send1"]), true);
        // Push peer_where = W_SRV|W_REC: recv is W_REC (kept), send is W_SND
        // (dropped).
        assert_eq!(arg.as_deref(), Some("--debug=recv"));

        let pull = make_output_option(OutputWordKind::Debug, &os(&["recv1", "send1"]), false);
        // Pull peer_where = W_SRV|W_SND: send kept, recv dropped.
        assert_eq!(pull.as_deref(), Some("--debug=send"));
    }
}
