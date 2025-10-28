use core::fmt::{self, Write as FmtWrite};

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
