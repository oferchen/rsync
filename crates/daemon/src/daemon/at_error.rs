//! Typed formatter for the daemon's pre-handshake `@ERROR:` protocol lines.
//!
//! Upstream rsync emits every daemon refusal in the ASCII negotiation phase as
//! `io_printf(f_out, "@ERROR: <msg>\n")` and then closes the socket; the client
//! treats a line beginning with `@ERROR` as fatal
//! (upstream: clientserver.c:381-385). Historically each refusal site in this
//! crate built that wire line with an ad-hoc `format!("@ERROR: ...")` and a
//! separate `\n`, so the `@ERROR: ` prefix and the terminating newline were
//! spelled out in a dozen places with no single point that guaranteed the exact
//! bytes.
//!
//! [`AtError`] is that single point. Each variant maps to one upstream message;
//! the message wording stays single-sourced in the `*_PAYLOAD` constants in the
//! parent module (which the upstream-wording pin tests guard), while this type
//! owns the `@ERROR: ` prefix and the trailing `\n`. The inverse is the client's
//! own parse path (`protocol::parse_legacy_error_message`), so the round-trip is
//! testable end to end.
//!
//! Out of scope: the read-only / write-only module rejections
//! (`ERROR: module is ...`, no `@`) and the incomplete-build placeholder, which
//! travel inside multiplexed `MSG_ERROR` frames without a trailing newline
//! rather than as raw negotiation-phase lines.

use std::borrow::Cow;

use super::{
    ACCESS_DENIED_PAYLOAD, AUTH_FAILED_PAYLOAD, CHDIR_FAILED_PAYLOAD, CHROOT_FAILED_PAYLOAD,
    INVALID_GID_PAYLOAD, INVALID_UID_PAYLOAD, MODULE_LOCK_ERROR_PAYLOAD,
    MODULE_MAX_CONNECTIONS_PAYLOAD, PROTOCOL_STARTUP_ERROR_PAYLOAD, SETGID_FAILED_PAYLOAD,
    SETGROUPS_FAILED_PAYLOAD, SETUID_FAILED_PAYLOAD, UNKNOWN_COMMAND_PAYLOAD,
    UNKNOWN_MODULE_PAYLOAD, UNSUPPORTED_AUTH_DIGEST_PAYLOAD,
};

/// The literal prefix every `@ERROR:` negotiation line carries.
///
/// upstream: clientserver.c - the format string is always `"@ERROR: %s\n"`.
pub(crate) const AT_ERROR_PREFIX: &str = "@ERROR: ";

/// A daemon `@ERROR:` refusal line, rendered to the wire as
/// `@ERROR: <message>\n` and parsed back by the client's
/// `protocol::parse_legacy_error_message`.
///
/// Variants carry only the dynamic operands; the fixed wording is taken from
/// the upstream-cited `*_PAYLOAD` constants so there is exactly one source of
/// truth for both the text (the constants) and the framing (this type).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AtError {
    /// upstream: clientserver.c:180-184 - the client's first line is not a
    /// version banner (`@ERROR: protocol startup error`).
    ProtocolStartupError,
    /// upstream: clientserver.c:732 - requested module does not exist, and the
    /// non-listable masking of an access-denied module
    /// (`@ERROR: Unknown module '<name>'`).
    UnknownModule(String),
    /// upstream: clientserver.c:1429 - a `#`-prefixed request that is neither
    /// `#list` nor `#early_input=` (`@ERROR: Unknown command '<line>'`).
    UnknownCommand(String),
    /// upstream: clientserver.c:735-736 - host denied access to a listable
    /// module (`@ERROR: access denied to <module> from <host> (<addr>)`).
    AccessDenied {
        /// Module name, sanitized for display.
        module: String,
        /// Reverse-resolved peer hostname, or the address when unavailable.
        host: String,
        /// Peer address in string form.
        addr: String,
    },
    /// upstream: clientserver.c:764 - authentication failed on a protected
    /// module (`@ERROR: auth failed on module <name>`).
    AuthFailed {
        /// Module name, sanitized for display.
        module: String,
    },
    /// upstream: compat.c:872-874 - none of the client's daemon-auth digests is
    /// supported here (`@ERROR: your client does not support ...: <list>`).
    UnsupportedAuthDigest {
        /// This build's advertised daemon-auth digest list.
        digests: String,
    },
    /// upstream: clientserver.c:754 - a module reached its connection cap
    /// (`@ERROR: max connections (<n>) reached -- try again later`). The count
    /// is rendered verbatim so a negative `max connections` keeps its sign.
    MaxConnections {
        /// The configured limit, rendered with upstream's `%d`.
        limit: i64,
    },
    /// upstream: clientserver.c:750 - opening the module lock file failed
    /// (`@ERROR: failed to open lock file`).
    ModuleLockError,
    /// upstream: clientserver.c:985 - chroot failed during module setup.
    ChrootFailed,
    /// upstream: clientserver.c:649 - chdir failed during module setup.
    ChdirFailed,
    /// upstream: clientserver.c:1053 - setuid failed after chroot.
    SetuidFailed,
    /// upstream: clientserver.c:1024 - setgid failed after chroot.
    SetgidFailed,
    /// upstream: clientserver.c:1031 - setgroups failed after chroot.
    SetgroupsFailed,
    /// upstream: clientserver.c:785 - a module uid NAME failed to resolve
    /// (`@ERROR: invalid uid <name>`).
    InvalidUid(String),
    /// upstream: clientserver.c:658 - a module gid NAME failed to resolve
    /// (`@ERROR: invalid gid <name>`).
    InvalidGid(String),
    /// A refusal whose body is composed at the call site (client-omitted
    /// greeting tokens, refused options, per-session setup failures that carry
    /// an OS error string). The body is stored WITHOUT the `@ERROR: ` prefix;
    /// [`AtError`] prepends it exactly once.
    Message(String),
}

impl AtError {
    /// Builds an [`AtError::Message`] from an already-composed body, stripping a
    /// redundant leading `@ERROR: ` if the caller passed a full line.
    pub(crate) fn message(body: impl Into<String>) -> Self {
        let body = body.into();
        match body.strip_prefix(AT_ERROR_PREFIX) {
            Some(rest) => Self::Message(rest.to_owned()),
            None => Self::Message(body),
        }
    }

    /// Renders the full `@ERROR: <message>` line, without a trailing newline.
    ///
    /// Fixed-wording variants borrow their `*_PAYLOAD` constant directly, so the
    /// emitted bytes are identical to the constant the pin tests guard.
    pub(crate) fn line(&self) -> Cow<'static, str> {
        match self {
            Self::ProtocolStartupError => Cow::Borrowed(PROTOCOL_STARTUP_ERROR_PAYLOAD),
            Self::ModuleLockError => Cow::Borrowed(MODULE_LOCK_ERROR_PAYLOAD),
            Self::ChrootFailed => Cow::Borrowed(CHROOT_FAILED_PAYLOAD),
            Self::ChdirFailed => Cow::Borrowed(CHDIR_FAILED_PAYLOAD),
            Self::SetuidFailed => Cow::Borrowed(SETUID_FAILED_PAYLOAD),
            Self::SetgidFailed => Cow::Borrowed(SETGID_FAILED_PAYLOAD),
            Self::SetgroupsFailed => Cow::Borrowed(SETGROUPS_FAILED_PAYLOAD),
            Self::UnknownModule(module) => {
                Cow::Owned(UNKNOWN_MODULE_PAYLOAD.replace("{module}", module))
            }
            Self::UnknownCommand(command) => {
                Cow::Owned(UNKNOWN_COMMAND_PAYLOAD.replace("{command}", command))
            }
            Self::AccessDenied { module, host, addr } => Cow::Owned(
                ACCESS_DENIED_PAYLOAD
                    .replace("{module}", module)
                    .replace("{host}", host)
                    .replace("{addr}", addr),
            ),
            Self::AuthFailed { module } => {
                Cow::Owned(AUTH_FAILED_PAYLOAD.replace("{module}", module))
            }
            Self::UnsupportedAuthDigest { digests } => {
                Cow::Owned(UNSUPPORTED_AUTH_DIGEST_PAYLOAD.replace("{digests}", digests))
            }
            Self::MaxConnections { limit } => {
                Cow::Owned(MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.to_string()))
            }
            Self::InvalidUid(name) => Cow::Owned(INVALID_UID_PAYLOAD.replace("{uid}", name)),
            Self::InvalidGid(name) => Cow::Owned(INVALID_GID_PAYLOAD.replace("{gid}", name)),
            Self::Message(body) => Cow::Owned(format!("{AT_ERROR_PREFIX}{body}")),
        }
    }

    /// Renders the exact wire bytes: `@ERROR: <message>\n`.
    ///
    /// upstream: every `io_printf(f_out, "@ERROR: ...\n")` emits the message
    /// followed by a single newline and nothing more.
    pub(crate) fn to_wire(&self) -> Vec<u8> {
        let mut bytes = self.line().into_owned().into_bytes();
        bytes.push(b'\n');
        bytes
    }
}

impl std::fmt::Display for AtError {
    /// Formats the `@ERROR:` line without the trailing newline, so the value can
    /// be logged or asserted on directly. Use [`AtError::to_wire`] for the bytes
    /// sent to the client.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.line())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::parse_legacy_error_message;

    /// Every upstream `@ERROR:` variant paired with the exact message body the
    /// client must recover, per clientserver.c / compat.c.
    fn upstream_variants() -> Vec<(AtError, &'static str)> {
        vec![
            (AtError::ProtocolStartupError, "protocol startup error"),
            (
                AtError::UnknownModule("secret".to_owned()),
                "Unknown module 'secret'",
            ),
            (
                AtError::UnknownCommand("#bogus".to_owned()),
                "Unknown command '#bogus'",
            ),
            (
                AtError::AccessDenied {
                    module: "public".to_owned(),
                    host: "myhost".to_owned(),
                    addr: "10.0.0.1".to_owned(),
                },
                "access denied to public from myhost (10.0.0.1)",
            ),
            (
                AtError::AuthFailed {
                    module: "protected".to_owned(),
                },
                "auth failed on module protected",
            ),
            (
                AtError::UnsupportedAuthDigest {
                    digests: "md5 md4".to_owned(),
                },
                "your client does not support one of our daemon-auth checksums: md5 md4",
            ),
            (
                AtError::MaxConnections { limit: 2 },
                "max connections (2) reached -- try again later",
            ),
            (AtError::ModuleLockError, "failed to open lock file"),
            (AtError::ChrootFailed, "chroot failed"),
            (AtError::ChdirFailed, "chdir failed"),
            (AtError::SetuidFailed, "setuid failed"),
            (AtError::SetgidFailed, "setgid failed"),
            (AtError::SetgroupsFailed, "setgroups failed"),
            (
                AtError::InvalidUid("nobody".to_owned()),
                "invalid uid nobody",
            ),
            (
                AtError::InvalidGid("nobody".to_owned()),
                "invalid gid nobody",
            ),
            (
                AtError::message("your client omitted the subprotocol value: @RSYNCD: 32"),
                "your client omitted the subprotocol value: @RSYNCD: 32",
            ),
        ]
    }

    /// Golden + round-trip: each variant renders the exact upstream
    /// `@ERROR: <body>\n` bytes, and feeding those bytes back through the
    /// client's parse path recovers the original body verbatim. This pins that
    /// the formatter and the parser are true inverses, so a refusal the daemon
    /// emits is always one the client decodes to the same message.
    #[test]
    fn every_variant_formats_and_parses_back() {
        for (at, body) in upstream_variants() {
            let expected_line = format!("@ERROR: {body}");
            assert_eq!(at.line(), expected_line, "line mismatch for {at:?}");
            assert_eq!(at.to_string(), expected_line, "Display mismatch for {at:?}");

            let wire = at.to_wire();
            assert_eq!(
                wire,
                format!("{expected_line}\n").into_bytes(),
                "wire bytes mismatch for {at:?}"
            );
            assert_eq!(wire.last(), Some(&b'\n'), "must end with a single newline");
            assert_ne!(
                wire.get(wire.len().wrapping_sub(2)),
                Some(&b'\n'),
                "must not double-terminate"
            );

            let line = std::str::from_utf8(&wire).expect("utf8");
            // Parse via the client's @ERROR parse path.
            let recovered = parse_legacy_error_message(line).expect("client parses @ERROR line");
            assert_eq!(recovered, body, "round-trip lost the body for {at:?}");
        }
    }

    /// `message()` never double-prefixes when a caller hands it a full line.
    #[test]
    fn message_strips_redundant_prefix() {
        let from_body = AtError::message("invalid daemon param: bad");
        let from_line = AtError::message("@ERROR: invalid daemon param: bad");
        assert_eq!(from_body, from_line);
        assert_eq!(from_body.line(), "@ERROR: invalid daemon param: bad");
    }

    /// A non-`@ERROR` line is not decoded as one by the client's parse path.
    #[test]
    fn parse_rejects_non_error_lines() {
        assert_eq!(parse_legacy_error_message("@RSYNCD: OK"), None);
    }
}
