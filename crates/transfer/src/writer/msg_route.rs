//! Where a server's diagnostic lines go: framed to the client, or written to
//! the server's own stderr.
//!
//! Upstream decides this once per process with the `msgs2stderr` global and
//! applies it in one place, `rwrite()`. A server over a remote shell writes its
//! error and warning lines to its own stderr by default - the remote shell
//! hands that stream to the client's terminal - and frames only `FINFO`. The
//! client takes the server's exit status from the remote-shell child, so an
//! error that never travels as a frame still ends the run with the server's
//! code.
//!
//! # Upstream Reference
//!
//! - `options.c:103` - `int msgs2stderr = 2;` (`--stderr=errors`).
//! - `options.c:2059-2060` - any explicit setting sets `saw_stderr_opt`.
//! - `compat.c:653-654` - `if (!saw_stderr_opt && protocol_version <= 28 &&
//!   am_server) msgs2stderr = 0;`
//! - `clientserver.c:1177-1178` - a daemon not run over a remote shell
//!   (`am_daemon > 0`) sets `msgs2stderr = 0`.
//! - `log.c:357` - `if (am_server && msgs2stderr != 1 && (msgs2stderr != 2 ||
//!   f != stderr))` frames the line; otherwise it is written locally.
//! - `main.c:1405` - the client's `wait_process_with_flush(pid, &exit_code)`.

use protocol::{MessageCode, ProtocolVersion};

/// Upstream's `msgs2stderr`: which of a server's diagnostic lines it writes to
/// its own stderr instead of framing them for the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageRoute {
    /// `msgs2stderr = 0` (`--no-msgs2stderr`): every line is framed.
    #[default]
    Frames,
    /// `msgs2stderr = 2`, the default: error and warning lines go to stderr,
    /// `FINFO` is framed.
    ErrorsToStderr,
    /// `msgs2stderr = 1` (`--msgs2stderr`): every line goes to stderr.
    AllToStderr,
}

impl MessageRoute {
    /// The route a server argument selects, or `None` for any other argument.
    ///
    /// upstream: options.c:619-620 - `--msgs2stderr` sets 1, `--no-msgs2stderr`
    /// sets 0.
    #[must_use]
    pub fn from_server_flag(flag: &str) -> Option<Self> {
        match flag {
            "--msgs2stderr" => Some(Self::AllToStderr),
            "--no-msgs2stderr" => Some(Self::Frames),
            _ => None,
        }
    }

    /// The route a server uses once the protocol is known: the requested one,
    /// or the default - framed below protocol 29, where the client may not
    /// have set up a stderr for the server, errors to stderr otherwise.
    ///
    /// upstream: compat.c:653-654, options.c:2059-2060.
    #[must_use]
    pub fn resolve(requested: Option<Self>, protocol: ProtocolVersion) -> Self {
        requested.unwrap_or(if protocol.as_u8() <= 28 {
            Self::Frames
        } else {
            Self::ErrorsToStderr
        })
    }

    /// Whether a line of `code` is written to this process's stderr.
    ///
    /// upstream: log.c:337-345 sends `FERROR*`/`FWARNING` to `stderr` and
    /// `FINFO` to `stdout`; log.c:357 frames unless `msgs2stderr == 1`, or
    /// `msgs2stderr == 2` and the line is bound for `stderr`.
    #[must_use]
    pub fn writes_locally(self, code: MessageCode) -> bool {
        match self {
            Self::Frames => false,
            Self::ErrorsToStderr => code != MessageCode::Info,
            Self::AllToStderr => true,
        }
    }
}

/// The frame code a line of `code` travels as for a peer speaking `protocol`.
///
/// upstream: log.c:359-364 - below protocol 30 `MSG_ERROR` becomes
/// `MSG_ERROR_XFER` and `MSG_WARNING` becomes `MSG_INFO`, the only codes the
/// older peer knows. The remap follows the stderr decision, so it never
/// changes where a line goes.
#[must_use]
pub fn frame_code(code: MessageCode, protocol: ProtocolVersion) -> MessageCode {
    if protocol.as_u8() >= 30 {
        return code;
    }
    match code {
        MessageCode::Error => MessageCode::ErrorXfer,
        MessageCode::Warning => MessageCode::Info,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn protocol(v: u8) -> ProtocolVersion {
        match v {
            28 => ProtocolVersion::V28,
            29 => ProtocolVersion::V29,
            30 => ProtocolVersion::V30,
            31 => ProtocolVersion::V31,
            _ => unreachable!("fixture protocols are 28-31"),
        }
    }

    /// A server over a remote shell keeps its errors off the wire by default;
    /// protocol 28 and older frame them, because that client may have no
    /// stderr wired up for the server; an explicit request always wins.
    ///
    /// upstream: options.c:103, compat.c:653-654, options.c:2059-2060.
    #[test]
    fn resolve_mirrors_the_default_the_protocol_floor_and_an_explicit_request() {
        assert_eq!(
            MessageRoute::resolve(None, protocol(31)),
            MessageRoute::ErrorsToStderr
        );
        assert_eq!(
            MessageRoute::resolve(None, protocol(29)),
            MessageRoute::ErrorsToStderr
        );
        assert_eq!(
            MessageRoute::resolve(None, protocol(28)),
            MessageRoute::Frames
        );
        for p in [28, 31] {
            for route in [MessageRoute::Frames, MessageRoute::AllToStderr] {
                assert_eq!(MessageRoute::resolve(Some(route), protocol(p)), route);
            }
        }
    }

    /// log.c:357 - only `msgs2stderr == 1` takes FINFO off the wire; the
    /// default takes every error and warning code off it.
    #[test]
    fn writes_locally_follows_rwrite() {
        use MessageCode::{Error, ErrorXfer, Info, Warning};
        for code in [Error, ErrorXfer, Warning] {
            assert!(!MessageRoute::Frames.writes_locally(code));
            assert!(MessageRoute::ErrorsToStderr.writes_locally(code));
            assert!(MessageRoute::AllToStderr.writes_locally(code));
        }
        assert!(!MessageRoute::Frames.writes_locally(Info));
        assert!(!MessageRoute::ErrorsToStderr.writes_locally(Info));
        assert!(MessageRoute::AllToStderr.writes_locally(Info));
    }

    /// log.c:359-364.
    #[test]
    fn frame_code_downgrades_only_below_protocol_30() {
        use MessageCode::{Error, ErrorXfer, Info, Warning};
        assert_eq!(frame_code(Error, protocol(29)), ErrorXfer);
        assert_eq!(frame_code(Warning, protocol(29)), Info);
        assert_eq!(frame_code(ErrorXfer, protocol(29)), ErrorXfer);
        for code in [Error, ErrorXfer, Warning, Info] {
            assert_eq!(frame_code(code, protocol(30)), code);
        }
    }

    #[test]
    fn from_server_flag_reads_only_the_two_forwarded_flags() {
        assert_eq!(
            MessageRoute::from_server_flag("--msgs2stderr"),
            Some(MessageRoute::AllToStderr)
        );
        assert_eq!(
            MessageRoute::from_server_flag("--no-msgs2stderr"),
            Some(MessageRoute::Frames)
        );
        assert_eq!(MessageRoute::from_server_flag("--stderr=all"), None);
    }
}
