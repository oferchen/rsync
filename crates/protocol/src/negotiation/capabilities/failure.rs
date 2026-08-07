//! The `recv_negotiate_str()` failure diagnostic (upstream `compat.c:369-406`).
//!
//! When a checksum or compression list has no mutually acceptable entry,
//! upstream prints a fixed block to `FERROR` and aborts with
//! `RERR_UNSUPPORTED`. This module builds that block byte-for-byte so both the
//! negotiated selection path ([`super::negotiate`]) and the non-negotiated
//! env-default check ([`super::env_list`]) share a single implementation.

use std::io;

/// Builds the [`io::Error`] upstream's `recv_negotiate_str()` raises when a
/// `%s` list yields no mutually acceptable choice (`compat.c:369-406`).
///
/// The message text is emitted verbatim as the `FERROR` diagnostic; the core
/// exit-code mapper turns [`io::ErrorKind::Unsupported`] into
/// `exit_cleanup(RERR_UNSUPPORTED)` (exit 4, `compat.c:406`).
///
/// Upstream prints three lines, but the two list-detail lines are guarded by
/// `if (!am_server || !do_negotiated_strings)` (`compat.c:382`):
///
/// ```text
/// Failed to negotiate a <kind> choice.
/// <peer> list: <peer_list>
/// <self> list:<own list rebuilt from saw>
/// ```
///
/// - `<peer>` is `Client` on the server and `Server` on the client
///   (`compat.c:384` `am_server ? "Client" : "Server"`); `peer_list` is the
///   list received from - or, on the non-negotiated path, the default assumed
///   of - the peer (upstream's `tmpbuf`).
/// - `<self>` is the opposite label (`compat.c:405`); its list is our own
///   candidate order with each name space-prefixed (`compat.c:394-402`), or
///   ` INVALID` when no candidate survived (`compat.c:403-404`).
///
/// On the server's negotiated path (`am_server && do_negotiated_strings`) the
/// two list lines are suppressed, matching upstream, so the server aborts on
/// the `Failed to negotiate ...` line alone.
pub(super) fn negotiation_failure(
    kind: &str,
    is_server: bool,
    do_negotiated: bool,
    peer_list: &str,
    own_candidates: &[&str],
) -> io::Error {
    // upstream: compat.c:387 rprintf(FERROR, "Failed to negotiate a %s choice.\n", ...)
    let mut msg = format!("Failed to negotiate a {kind} choice.");

    // upstream: compat.c:384 - only `!am_server || !do_negotiated_strings`
    // prints the offered lists.
    if !is_server || !do_negotiated {
        let (peer_label, own_label) = if is_server {
            ("Client", "Server")
        } else {
            ("Server", "Client")
        };

        // upstream: compat.c:388 rprintf(FERROR, "%s list: %s\n", peer, tmpbuf)
        msg.push('\n');
        msg.push_str(peer_label);
        msg.push_str(" list: ");
        msg.push_str(peer_list);

        // upstream: compat.c:394-402 - rebuild our own list from the saw
        // ordinals, each surviving name written with a leading space.
        let mut own = String::new();
        for name in own_candidates {
            own.push(' ');
            own.push_str(name);
        }
        // upstream: compat.c:403-404 - an empty rebuild becomes " INVALID".
        if own.is_empty() {
            own.push_str(" INVALID");
        }

        // upstream: compat.c:405 rprintf(FERROR, "%s list:%s\n", self, tmpbuf) -
        // no space after the colon; the rebuilt list already leads with one.
        msg.push('\n');
        msg.push_str(own_label);
        msg.push_str(" list:");
        msg.push_str(&own);
    }

    io::Error::new(io::ErrorKind::Unsupported, msg)
}
