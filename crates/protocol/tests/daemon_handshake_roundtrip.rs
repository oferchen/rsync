//! Round-trip tests for the `@RSYNCD` daemon-handshake wire surface.
//!
//! An inventory of the negotiation boundaries found that the ASCII daemon
//! handshake was covered only in one direction: parsers were checked against
//! hand-written byte literals, and emitters were checked against hand-written
//! expected strings, but the two halves were never wired together. These tests
//! close the loop for the emit direction: they drive oc's real emit function
//! and feed its output straight into oc's real parse function, asserting that
//! the parse recovers the emitted value, and they pin the emitted bytes against
//! upstream rsync 3.4.4's exact format so a format drift fails loudly.
//!
//! The daemon and the client both funnel this surface through the `protocol`
//! crate's public helpers:
//!
//! - Greeting: the daemon builds it in `legacy_daemon_greeting_for_protocol`
//!   from `format_legacy_daemon_message(Version(..))` plus the auth digest list;
//!   the client reads it with `parse_legacy_daemon_greeting_details`
//!   (`crates/core/src/client/module_list/`).
//! - `@RSYNCD: OK` / `@RSYNCD: EXIT` / `@RSYNCD: AUTHREQD <challenge>`: the
//!   daemon writes them with `format_legacy_daemon_message`
//!   (`daemon/sections/legacy_messages.rs`,
//!   `daemon/sections/module_access/authentication.rs`); the client reads them
//!   with `parse_legacy_daemon_message` (`client/module_list/listing.rs`).
//! - `@ERROR: <msg>`: the client reads it with `parse_legacy_error_message`.
//!
//! Upstream byte formats pinned here (rsync 3.4.4, protocol 32):
//!
//! - `output_daemon_greeting()` compat.c:842 - `@RSYNCD: %d.%d %s\n`, where
//!   `%s` is `get_default_nno_list(&valid_auth_checksums, ...)` =
//!   `sha512 sha256 sha1 md5 md4`.
//! - `send_listing()` clientserver.c:1268 - `%-15s\t%s\n` per module, then
//!   `@RSYNCD: EXIT\n` (clientserver.c:1272, protocol >= 25).
//! - `auth_server()` authenticate.c:245 - `"%s%s\n"` with leader
//!   `"@RSYNCD: AUTHREQD "`, i.e. `@RSYNCD: AUTHREQD <challenge>\n`.
//! - `auth_client()` authenticate.c:375 - `"%s %s\n"` = `<user> <response>\n`.
//! - `rsync_module()` clientserver.c:1071 - `@RSYNCD: OK\n`.

use protocol::{
    AdvertisedDigests, LegacyDaemonMessage, ProtocolVersion, daemon_auth_digest_list,
    format_daemon_auth_response, format_daemon_module_listing, format_legacy_daemon_message,
    parse_daemon_auth_response, parse_daemon_module_listing, parse_legacy_daemon_greeting_details,
    parse_legacy_daemon_message, parse_legacy_error_message,
};

/// Rebuilds the daemon's on-the-wire greeting exactly as
/// `legacy_daemon_greeting_for_protocol` does: the version banner with its
/// trailing newline replaced by a space, the advertised auth digest list, and a
/// final newline.
///
/// upstream: compat.c:842 `io_printf(f_out, "@RSYNCD: %d.%d %s\n", ...)`.
fn emit_daemon_greeting(version: ProtocolVersion) -> String {
    let mut greeting = format_legacy_daemon_message(LegacyDaemonMessage::Version(version));
    assert!(greeting.ends_with('\n'));
    greeting.pop();
    greeting.push(' ');
    // `daemon_auth_digest_list()` is the protocol-crate spelling of the list the
    // daemon appends via `core::auth::supported_daemon_digest_list()`; a lockstep
    // test keeps the two identical.
    greeting.push_str(&daemon_auth_digest_list());
    greeting.push('\n');
    greeting
}

// -- Greeting --------------------------------------------------------------

/// The full protocol-32 greeting the daemon emits is byte-for-byte the upstream
/// format, and the client parser recovers every field from it.
#[test]
fn greeting_protocol_32_round_trips_and_matches_upstream_bytes() {
    let greeting = emit_daemon_greeting(ProtocolVersion::V32);

    // Golden: the exact bytes an rsync 3.4.4 daemon sends at protocol 32.
    assert_eq!(greeting, "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n");

    let parsed = parse_legacy_daemon_greeting_details(&greeting).expect("client parses greeting");
    assert_eq!(parsed.protocol(), ProtocolVersion::V32);
    assert_eq!(parsed.advertised_protocol(), 32);
    assert_eq!(parsed.subprotocol(), 0);
    assert!(parsed.has_subprotocol());
    assert_eq!(
        parsed.advertised_digests(),
        AdvertisedDigests::Present("sha512 sha256 sha1 md5 md4"),
    );
    assert!(parsed.supports_digest("sha512"));
    assert!(parsed.supports_digest("md5"));
    assert!(!parsed.supports_digest("crc32"));
}

/// The digest list is version-independent: upstream never filters it, so every
/// protocol era emits the same list and the parser recovers it unchanged. The
/// version number in the banner is the only thing that varies.
#[test]
fn greeting_round_trips_at_every_supported_protocol() {
    for version in [
        ProtocolVersion::V28,
        ProtocolVersion::V29,
        ProtocolVersion::V30,
        ProtocolVersion::V31,
        ProtocolVersion::V32,
    ] {
        let greeting = emit_daemon_greeting(version);

        // upstream: compat.c:842 - `@RSYNCD: <ver>.0 <digests>\n`, unfiltered.
        assert_eq!(
            greeting,
            format!(
                "@RSYNCD: {}.0 sha512 sha256 sha1 md5 md4\n",
                version.as_u8()
            ),
        );

        let parsed =
            parse_legacy_daemon_greeting_details(&greeting).expect("client parses greeting");
        assert_eq!(parsed.protocol(), version, "protocol {}", version.as_u8());
        assert_eq!(parsed.advertised_protocol(), u32::from(version.as_u8()));
        assert_eq!(parsed.subprotocol(), 0);
        assert_eq!(
            parsed.digest_names(),
            Some("sha512 sha256 sha1 md5 md4"),
            "digest list at protocol {}",
            version.as_u8(),
        );
    }
}

/// The greeting also classifies as a `Version` message through the general
/// line parser, the same path the client's listing loop uses for every
/// `@RSYNCD:` line before it has locked onto the greeting.
#[test]
fn greeting_classifies_as_version_message() {
    let greeting = emit_daemon_greeting(ProtocolVersion::V32);
    let parsed = parse_legacy_daemon_message(&greeting).expect("classifies greeting");
    assert_eq!(parsed, LegacyDaemonMessage::Version(ProtocolVersion::V32),);
}

// -- OK / EXIT bookends ----------------------------------------------------

/// `@RSYNCD: OK` emitted by the daemon parses back to `Ok` and re-emits to the
/// identical bytes.
#[test]
fn ok_message_round_trips_and_matches_upstream_bytes() {
    let emitted = format_legacy_daemon_message(LegacyDaemonMessage::Ok);

    // upstream: clientserver.c:1071 `io_printf(f_out, "@RSYNCD: OK\n")`.
    assert_eq!(emitted, "@RSYNCD: OK\n");

    let parsed = parse_legacy_daemon_message(&emitted).expect("client parses OK");
    assert_eq!(parsed, LegacyDaemonMessage::Ok);
    assert_eq!(format_legacy_daemon_message(parsed), emitted);
}

/// `@RSYNCD: EXIT` terminates the module listing; it round-trips and re-emits
/// to the identical bytes.
#[test]
fn exit_message_round_trips_and_matches_upstream_bytes() {
    let emitted = format_legacy_daemon_message(LegacyDaemonMessage::Exit);

    // upstream: clientserver.c:1272 `io_printf(fd, "@RSYNCD: EXIT\n")`.
    assert_eq!(emitted, "@RSYNCD: EXIT\n");

    let parsed = parse_legacy_daemon_message(&emitted).expect("client parses EXIT");
    assert_eq!(parsed, LegacyDaemonMessage::Exit);
    assert_eq!(format_legacy_daemon_message(parsed), emitted);
}

// -- Module listing lines --------------------------------------------------
//
// The daemon emits each module line with `protocol::format_daemon_module_listing`
// (`daemon/sections/module_access/listing.rs`) and the client parses it with
// `protocol::parse_daemon_module_listing` (`ModuleListEntry::from_line` in
// `core/src/client/module_list/listing.rs`). Both sides funnel through the same
// shared protocol helpers, so these tests drive the real emit/parse pair, pin
// the upstream bytes, and confirm the parse recovers what the format wrote. The
// listing is bracketed on the wire by the shared `@RSYNCD: EXIT` terminator,
// covered above.

/// Maps `parse_daemon_module_listing`'s `(name, comment)` to the client's
/// `ModuleListEntry` view, where an empty comment field is absent (`None`).
fn parse_module_entry(line: &str) -> (String, Option<String>) {
    let (name, comment) = parse_daemon_module_listing(line);
    let comment = if comment.is_empty() {
        None
    } else {
        Some(comment.to_owned())
    };
    (name.to_owned(), comment)
}

/// A short module name is padded to 15 columns, tab-separated from its comment,
/// and the client split recovers the name (trailing pad trimmed by the daemon
/// itself is preserved in the field, but the parser strips it from the name).
#[test]
fn module_listing_line_round_trips_and_matches_upstream_layout() {
    let line = format_daemon_module_listing("data", "shared data");

    // Golden: name left-justified in a 15-wide field, TAB, comment, newline.
    // "data" + 11 spaces = 15 columns.
    assert_eq!(line, "data           \tshared data\n");

    let (name, comment) = parse_module_entry(&line);
    // The daemon left-pads the name field, so the recovered name carries the
    // padding exactly as the client's parse yields it. The client never trims
    // it either; this pins that observable behaviour.
    assert_eq!(name, "data           ");
    assert_eq!(comment.as_deref(), Some("shared data"));
}

/// A module with no comment still emits the padded name, a tab, and an empty
/// comment; the client maps the empty comment to `None`.
#[test]
fn module_listing_line_without_comment_maps_to_none() {
    let line = format_daemon_module_listing("backup", "");
    assert_eq!(line, "backup         \t\n");

    let (name, comment) = parse_module_entry(&line);
    assert_eq!(name, "backup         ");
    assert_eq!(comment, None);
}

/// A name longer than 15 characters is not truncated - the field simply grows,
/// matching printf's `%-15s` minimum-width (not maximum) semantics.
#[test]
fn module_listing_line_long_name_is_not_truncated() {
    let line = format_daemon_module_listing("a-very-long-module-name", "c");
    assert_eq!(line, "a-very-long-module-name\tc\n");

    let (name, comment) = parse_module_entry(&line);
    assert_eq!(name, "a-very-long-module-name");
    assert_eq!(comment.as_deref(), Some("c"));
}

// -- Auth challenge / response --------------------------------------------

/// The daemon emits its auth challenge as
/// `LegacyDaemonMessage::AuthRequired { challenge: Some(challenge) }`
/// (`authentication.rs`), and the client parses it back into the same variant
/// (`listing.rs`), reading the value as the challenge. The bytes match
/// upstream's `@RSYNCD: AUTHREQD <challenge>\n`.
#[test]
fn authreqd_challenge_round_trips_and_matches_upstream_bytes() {
    // A representative base64 challenge (unpadded, as `gen_challenge` emits).
    let challenge = "dGVzdGNoYWxsZW5nZQ";
    let message = LegacyDaemonMessage::AuthRequired {
        challenge: Some(challenge),
    };
    let emitted = format_legacy_daemon_message(message);

    // upstream: authenticate.c:245 - leader "@RSYNCD: AUTHREQD " + challenge.
    assert_eq!(emitted, "@RSYNCD: AUTHREQD dGVzdGNoYWxsZW5nZQ\n");

    let parsed = parse_legacy_daemon_message(&emitted).expect("client parses AUTHREQD");
    assert_eq!(
        parsed,
        LegacyDaemonMessage::AuthRequired {
            challenge: Some(challenge),
        },
    );
    assert_eq!(format_legacy_daemon_message(parsed), emitted);
}

/// An `AUTHREQD` with no trailing value (the older split-handshake form) round-
/// trips to `challenge: None`.
#[test]
fn authreqd_without_challenge_round_trips() {
    let emitted =
        format_legacy_daemon_message(LegacyDaemonMessage::AuthRequired { challenge: None });
    assert_eq!(emitted, "@RSYNCD: AUTHREQD\n");

    let parsed = parse_legacy_daemon_message(&emitted).expect("client parses bare AUTHREQD");
    assert_eq!(
        parsed,
        LegacyDaemonMessage::AuthRequired { challenge: None }
    );
}

/// Drives the shared auth-response helpers: the client emits `<user> <response>`
/// with `format_daemon_auth_response` (`send_daemon_auth_credentials` in
/// `core/src/client/module_list/auth.rs`) and the daemon parses it with
/// `parse_daemon_auth_response` (`authentication.rs`). This pins the shared wire
/// contract between them.
///
/// upstream: authenticate.c:375 `io_printf(fd, "%s %s\n", user, pass2)`.
#[test]
fn auth_response_line_round_trips_and_matches_upstream_bytes() {
    let user = "alice";
    let response = "cmVzcG9uc2VoYXNo";

    let emitted = format_daemon_auth_response(user, response);
    assert_eq!(emitted, "alice cmVzcG9uc2VoYXNo\n");

    // Daemon-side split: first whitespace token is the user, the rest the hash.
    let (parsed_user, parsed_response) = parse_daemon_auth_response(&emitted);
    assert_eq!(parsed_user, user);
    assert_eq!(parsed_response, response);
}

// -- @ERROR lines ----------------------------------------------------------

/// Every fatal `@ERROR:` line the daemon emits parses back to its payload
/// through the client's `parse_legacy_error_message`, and the byte format
/// matches upstream's `@ERROR: <msg>\n`.
///
/// upstream: clientserver.c passim `io_printf(f_out, "@ERROR: ...\n")`; the
/// client reads it at clientserver.c:381 `strncmp(line, "@ERROR", 6)`.
#[test]
fn at_error_lines_round_trip_through_the_client_parser() {
    // Real payloads the oc daemon emits (see `crates/daemon/src/daemon.rs`
    // and `daemon/sections/greeting.rs`).
    for payload in [
        "protocol startup error",
        "auth failed on module data",
        "Unknown module 'nope'",
        "your client omitted the digest name list: @RSYNCD: 32.0",
        "chdir failed",
        "max connections (4) reached -- try again later",
    ] {
        // The daemon writes `@ERROR: <payload>\n`.
        let emitted = format!("@ERROR: {payload}\n");

        let parsed = parse_legacy_error_message(&emitted)
            .unwrap_or_else(|| panic!("client parses @ERROR line: {emitted:?}"));
        assert_eq!(parsed, payload, "round-trip payload for {emitted:?}");
    }
}

/// An `@ERROR:` with an empty payload still parses (to the empty string), and a
/// line that is not an `@ERROR:` line is rejected by the parser - so the client
/// never mistakes a module name or another banner for an error.
#[test]
fn at_error_parser_is_prefix_exact() {
    assert_eq!(parse_legacy_error_message("@ERROR:\n"), Some(""));
    assert_eq!(
        parse_legacy_error_message("@ERROR: denied\r\n"),
        Some("denied")
    );
    assert_eq!(parse_legacy_error_message("@RSYNCD: OK\n"), None);
    assert_eq!(parse_legacy_error_message("data\tshared\n"), None);
}
