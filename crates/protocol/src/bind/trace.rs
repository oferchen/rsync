//! `--debug=BIND` producer emissions for daemon listener setup.
//!
//! Mirrors upstream rsync 3.4.1's `DEBUG_GTE(BIND, 1)` output byte-for-byte
//! so wire-comparable diagnostics align across implementations. Upstream
//! accumulates per-address-family error messages while iterating
//! `getaddrinfo` results inside `open_socket_in` and flushes them through
//! the `BIND` debug gate when at least one bind succeeded, or
//! unconditionally when every candidate failed.
//!
//! # Upstream Reference
//!
//! - `socket.c:432-438` `socket() failed:` - per-family accumulation when
//!   `socket(2)` returns `-1`.
//! - `socket.c:461-470` `bind() failed:` - per-family accumulation when
//!   `bind(2)` returns `-1`.
//! - `socket.c:479-486` - the flush loop that fires each message through
//!   `FLOG` once the iteration completes.
//! - `options.c:292` - `DEBUG_WORD(BIND, W_CLI, "Debug socket bind actions")`
//!   flag table entry. Useful upstream `DEBUG_GTE` calls cap at level 1.
//!
//! The helpers in this module fire the upstream-verbatim message text
//! through `debug_log!(Bind, 1, ...)`. Callsites that need the
//! unconditional `!i` flush (every candidate failed) should emit a
//! separate `FERROR`-equivalent line in addition to calling the helper,
//! matching `socket.c:488-494`.

use std::io;

use logging::debug_log;

/// Traces a failed `bind(2)` attempt during daemon listener setup.
///
/// upstream: `socket.c:463-465` -
/// `"bind() failed: %s (address-family %d)\n"`. `address_family` carries
/// the raw integer from `resp->ai_family` (typically `AF_INET = 2` or
/// `AF_INET6 = 10` on Linux). The error string mirrors what upstream
/// produces via `strerror(errno)`; we use `io::Error::to_string` which
/// resolves to the same OS message text on Unix targets.
///
/// The helper is gated internally on `DebugFlag::Bind` level 1, so
/// callsites pay no cost when BIND is disabled.
#[inline]
pub fn trace_bind_failure(address_family: i32, error: &io::Error) {
    debug_log!(
        Bind,
        1,
        "bind() failed: {} (address-family {})",
        error,
        address_family
    );
}

/// Traces a failed `socket(2)` attempt during daemon listener setup.
///
/// upstream: `socket.c:433-436` -
/// `"socket(%d,%d,%d) failed: %s\n"`. `family`, `socktype`, and
/// `protocol` carry the raw integers passed to the `socket(2)` syscall
/// (`resp->ai_family`, `resp->ai_socktype`, `resp->ai_protocol`).
///
/// The helper is gated internally on `DebugFlag::Bind` level 1, so
/// callsites pay no cost when BIND is disabled.
#[inline]
pub fn trace_socket_failure(family: i32, socktype: i32, protocol: i32, error: &io::Error) {
    debug_log!(
        Bind,
        1,
        "socket({},{},{}) failed: {}",
        family,
        socktype,
        protocol,
        error
    );
}

#[cfg(test)]
mod tests {
    //! Pinning tests for BIND emission shape. Strings match upstream
    //! `socket.c:433-436` and `socket.c:463-465` byte-for-byte once the
    //! `strerror(errno)` substitution is taken into account.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    /// Initialises logging with the requested BIND level and clears the
    /// pending event buffer so assertions can focus on emissions produced
    /// by the test body.
    fn init_bind(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.bind = level;
        init(cfg);
        let _ = drain_events();
    }

    /// Collects BIND debug messages emitted since the last drain.
    fn bind_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Bind,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Pins the level 1 `bind() failed:` emission shape against upstream
    /// `socket.c:463-465`.
    #[test]
    fn bind_failure_matches_upstream_format() {
        init_bind(1);
        let err = io::Error::from_raw_os_error(libc_eaddrinuse());
        trace_bind_failure(10, &err);
        let msgs = bind_messages();
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("bind() failed: ") && m.ends_with(" (address-family 10)")),
            "expected upstream-format BIND,1 bind() failure line, got {msgs:?}"
        );
    }

    /// Pins the level 1 `socket() failed:` emission shape against upstream
    /// `socket.c:433-436`.
    #[test]
    fn socket_failure_matches_upstream_format() {
        init_bind(1);
        let err = io::Error::from_raw_os_error(libc_eafnosupport());
        trace_socket_failure(10, 1, 6, &err);
        let msgs = bind_messages();
        assert!(
            msgs.iter()
                .any(|m| m.starts_with("socket(10,1,6) failed: ")),
            "expected upstream-format BIND,1 socket() failure line, got {msgs:?}"
        );
    }

    /// Level 0 suppresses every BIND emission, matching upstream's
    /// `DEBUG_GTE(BIND, _)` gate.
    #[test]
    fn level_zero_suppresses_all_bind_emissions() {
        init_bind(0);
        let err = io::Error::from_raw_os_error(libc_eaddrinuse());
        trace_bind_failure(2, &err);
        trace_socket_failure(2, 1, 6, &err);
        assert!(
            bind_messages().is_empty(),
            "all BIND emissions must be gated at level 0"
        );
    }

    /// Multiple invocations produce one emission per call, matching
    /// upstream's per-address-family accumulation behaviour.
    #[test]
    fn helpers_emit_one_line_per_call() {
        init_bind(1);
        let err = io::Error::from_raw_os_error(libc_eaddrinuse());
        trace_bind_failure(2, &err);
        trace_bind_failure(10, &err);
        assert_eq!(bind_messages().len(), 2);
    }

    /// Provides `EADDRINUSE` without pulling in the `libc` crate for tests.
    const fn libc_eaddrinuse() -> i32 {
        #[cfg(target_os = "linux")]
        {
            98
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            48
        }
        #[cfg(target_os = "windows")]
        {
            10048
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "windows"
        )))]
        {
            48
        }
    }

    /// Provides `EAFNOSUPPORT` without pulling in the `libc` crate for tests.
    const fn libc_eafnosupport() -> i32 {
        #[cfg(target_os = "linux")]
        {
            97
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            47
        }
        #[cfg(target_os = "windows")]
        {
            10047
        }
        #[cfg(not(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "ios",
            target_os = "windows"
        )))]
        {
            47
        }
    }
}
