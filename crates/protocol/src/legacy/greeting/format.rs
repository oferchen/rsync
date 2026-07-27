use ::core::fmt::{self, Write as FmtWrite};

use crate::version::ProtocolVersion;

use super::super::LEGACY_DAEMON_PREFIX;

/// Writes the legacy ASCII daemon greeting into the supplied [`fmt::Write`] sink.
///
/// Upstream daemons send a line such as `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n` when speaking to
/// older clients. The helper mirrors that layout without allocating, enabling
/// callers to render the greeting directly into stack buffers or
/// pre-allocated `String`s. The newline terminator is appended automatically to
/// match upstream rsync's behaviour.
pub fn write_legacy_daemon_greeting<W: FmtWrite>(
    writer: &mut W,
    version: ProtocolVersion,
) -> fmt::Result {
    writer.write_str(LEGACY_DAEMON_PREFIX)?;
    writer.write_char(' ')?;

    let mut value = version.as_u8();
    let mut digits = [0u8; 3];
    let mut len = 0usize;

    loop {
        debug_assert!(
            len < digits.len(),
            "protocol version must fit in three decimal digits"
        );
        digits[len] = value % 10;
        len += 1;
        value /= 10;

        if value == 0 {
            break;
        }
    }

    for index in (0..len).rev() {
        writer.write_char(char::from(b'0' + digits[index]))?;
    }

    writer.write_str(".0\n")
}

/// Formats the legacy ASCII daemon greeting used by pre-protocol-30 peers.
///
/// This convenience wrapper allocates a [`String`] and delegates to
/// [`write_legacy_daemon_greeting`] so existing call sites can retain their API
/// while newer code paths format directly into reusable buffers.
#[must_use]
pub fn format_legacy_daemon_greeting(version: ProtocolVersion) -> String {
    let mut banner = String::with_capacity(LEGACY_DAEMON_PREFIX.len() + 6);
    write_legacy_daemon_greeting(&mut banner, version).expect("writing to a String cannot fail");
    banner
}

/// Daemon-auth digest names this build advertises, in upstream table order.
///
/// upstream: checksum.c:71-84 `valid_auth_checksums_items[]`. This is the wire
/// spelling of the set, and the only place it is written down; `core::auth` maps
/// its `DaemonAuthDigest` preference order onto this table and a lockstep test
/// keeps the two from drifting.
///
/// The seeded `CSUM_MD4_OLD` variant is deliberately absent. It shares the `md4`
/// token with plain MD4 and exists only as the sub-protocol-30 substitute for an
/// *absent* list (compat.c:857-862), so advertising it would emit `md4` twice.
pub const DAEMON_AUTH_DIGEST_NAMES: [&str; 5] = ["sha512", "sha256", "sha1", "md5", "md4"];

/// Writes the advertised daemon-auth digest names, space separated, with no newline.
///
/// upstream: compat.c:462 `get_default_nno_list()` walks the table in order and
/// joins the names with a single space.
///
/// # Errors
///
/// Propagates any error reported by `writer`.
pub fn write_daemon_auth_digest_list<W: FmtWrite>(writer: &mut W) -> fmt::Result {
    for (index, name) in DAEMON_AUTH_DIGEST_NAMES.iter().enumerate() {
        if index > 0 {
            writer.write_char(' ')?;
        }
        writer.write_str(name)?;
    }

    Ok(())
}

/// Returns the daemon-auth digest list this build advertises in its `@RSYNCD:` greeting.
///
/// The list states *our own* capabilities and is never derived from the peer's
/// greeting, on either side of the connection: `output_daemon_greeting()`
/// (compat.c:838-842) renders `get_default_nno_list()` and is called before the
/// peer's greeting has even been read (clientserver.c:157 precedes the
/// `read_line_old()` at clientserver.c:174).
///
/// It is likewise never filtered by protocol version. Verified against rsync
/// 3.4.4: a client forced to `--protocol=28` still greets with
/// `@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4`.
#[must_use]
pub fn daemon_auth_digest_list() -> String {
    let mut list = String::with_capacity(32);
    write_daemon_auth_digest_list(&mut list).expect("writing to a String cannot fail");
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_protocol_32() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V32);
        assert!(greeting.starts_with(LEGACY_DAEMON_PREFIX));
        assert!(greeting.contains("32.0"));
        assert!(greeting.ends_with('\n'));
    }

    #[test]
    fn format_protocol_31() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V31);
        assert!(greeting.contains("31.0"));
    }

    #[test]
    fn format_protocol_30() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V30);
        assert!(greeting.contains("30.0"));
    }

    #[test]
    fn format_protocol_29() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V29);
        assert!(greeting.contains("29.0"));
    }

    #[test]
    fn greeting_starts_with_prefix() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V32);
        assert!(greeting.starts_with("@RSYNCD:"));
    }

    #[test]
    fn greeting_ends_with_newline() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V32);
        assert!(greeting.ends_with('\n'));
    }

    #[test]
    fn write_to_string_works() {
        let mut output = String::new();
        let result = write_legacy_daemon_greeting(&mut output, ProtocolVersion::V32);
        assert!(result.is_ok());
        assert!(!output.is_empty());
    }

    #[test]
    fn greeting_contains_version_and_minor() {
        let greeting = format_legacy_daemon_greeting(ProtocolVersion::V32);
        assert!(greeting.contains(".0"));
    }

    // upstream: checksum.c:71-84 joined by compat.c:462 `get_default_nno_list()`.
    // Measured against rsync 3.4.4 driven at a stub daemon: every greeting it
    // sends carries exactly this list, so the bytes are load-bearing.
    #[test]
    fn daemon_auth_digest_list_matches_upstream_bytes() {
        assert_eq!(daemon_auth_digest_list(), "sha512 sha256 sha1 md5 md4");
    }

    // The seeded CSUM_MD4_OLD shares the "md4" token with plain MD4, so a table
    // built by mapping over digest variants would advertise `md4` twice and
    // upstream's parser would see a duplicate this build never intends to offer.
    #[test]
    fn daemon_auth_digest_names_are_unique() {
        for (index, name) in DAEMON_AUTH_DIGEST_NAMES.iter().enumerate() {
            assert!(
                !DAEMON_AUTH_DIGEST_NAMES[..index].contains(name),
                "{name} is advertised more than once",
            );
        }
    }

    #[test]
    fn write_daemon_auth_digest_list_appends_without_newline() {
        let mut sink = String::from("@RSYNCD: 28.0 ");
        write_daemon_auth_digest_list(&mut sink).expect("writing to a String cannot fail");
        assert_eq!(sink, "@RSYNCD: 28.0 sha512 sha256 sha1 md5 md4");
    }
}
