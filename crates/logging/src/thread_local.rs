//! Thread-local storage for verbosity configuration and event collection.
//!
//! Upstream rsync stores `info_levels[]` and `debug_levels[]` as file-scope
//! globals (upstream: rsync.h). In a multi-threaded Rust process each thread
//! may run a separate transfer pipeline (sender, receiver, generator), so
//! this module uses `thread_local!` storage instead of a global `static`.
//!
//! The [`init`] function installs a [`VerbosityConfig`] for the calling
//! thread. The [`info_gte`] and [`debug_gte`] functions check levels against
//! that config - these are the Rust equivalents of upstream's `INFO_GTE()`
//! and `DEBUG_GTE()` macros (upstream: rsync.h).
//!
//! Events emitted via [`emit_info`] and [`emit_debug`] are buffered in a
//! thread-local `Vec<DiagnosticEvent>` and retrieved with [`drain_events`].
//! The caller routes them to stderr, the multiplexed I/O stream, or a log
//! file - mirroring upstream's `rwrite()` dispatch (upstream: log.c:rwrite()).

use super::config::VerbosityConfig;
use super::levels::{DebugFlag, InfoFlag};
use super::log_code::LogCode;
use super::sequence::Stamped;
use std::cell::RefCell;

thread_local! {
    static VERBOSITY: RefCell<VerbosityConfig> = RefCell::new(VerbosityConfig::default());
    /// Events are held stamped with their production order so a consumer that
    /// interleaves them with another output channel can recover that order.
    /// The buffer itself stays thread-local; only the ordering key is shared
    /// process-wide, which is what makes keys from two threads comparable.
    #[allow(clippy::missing_const_for_thread_local)]
    static EVENTS: RefCell<Vec<Stamped<DiagnosticEvent>>> = RefCell::new(Vec::new());
}

/// A diagnostic event collected during execution.
///
/// Events are buffered per-thread and drained by the caller for output.
/// The two variants correspond to the info and debug flag systems. Every
/// event additionally carries the upstream [`LogCode`] that classifies its
/// destination (upstream: log.c:rwrite() dispatches on `enum logcode`).
#[derive(Clone, Debug)]
pub enum DiagnosticEvent {
    /// Info-level diagnostic event.
    Info {
        /// The info flag category.
        flag: InfoFlag,
        /// The verbosity level.
        level: u8,
        /// The upstream log code classifying the event's destination.
        code: LogCode,
        /// The diagnostic message.
        message: String,
    },
    /// Debug-level diagnostic event.
    Debug {
        /// The debug flag category.
        flag: DebugFlag,
        /// The verbosity level.
        level: u8,
        /// The upstream log code classifying the event's destination.
        code: LogCode,
        /// The diagnostic message.
        message: String,
    },
}

impl DiagnosticEvent {
    /// Returns the upstream [`LogCode`] carried by this event.
    #[must_use]
    pub const fn code(&self) -> LogCode {
        match self {
            DiagnosticEvent::Info { code, .. } | DiagnosticEvent::Debug { code, .. } => *code,
        }
    }
}

/// Initialize verbosity configuration for the current thread.
///
/// Must be called once per thread before any logging macros are used.
/// Subsequent calls overwrite the previous configuration.
pub fn init(config: VerbosityConfig) {
    VERBOSITY.with(|v| {
        *v.borrow_mut() = config;
    });
}

/// Check if the info flag is at or above the specified level.
///
/// Equivalent to upstream's `INFO_GTE(flag, level)` macro (upstream: rsync.h).
pub fn info_gte(flag: InfoFlag, level: u8) -> bool {
    VERBOSITY.with(|v| v.borrow().info.get(flag) >= level)
}

/// Check if the debug flag is at or above the specified level.
///
/// Equivalent to upstream's `DEBUG_GTE(flag, level)` macro (upstream: rsync.h).
pub fn debug_gte(flag: DebugFlag, level: u8) -> bool {
    VERBOSITY.with(|v| v.borrow().debug.get(flag) >= level)
}

/// Emit an info diagnostic event into the thread-local buffer.
///
/// The caller is responsible for checking [`info_gte`] first; the
/// [`info_log!`](crate::info_log) macro handles this automatically.
///
/// The event is tagged [`LogCode::Info`], the code whose upstream routing
/// (client stdout, subject to verbosity) matches how the renderer treats
/// every drained event today. Producers that mirror a different upstream
/// `rprintf()` code use [`emit_info_coded`].
// upstream: log.c:rwrite() dispatches FINFO messages
pub fn emit_info(flag: InfoFlag, level: u8, message: String) {
    emit_info_coded(flag, level, LogCode::Info, message);
}

/// Emit an info diagnostic event carrying an explicit upstream [`LogCode`].
///
/// Use this when the producing site mirrors an upstream `rprintf(code, ...)`
/// whose code is not `FINFO` (upstream: log.c:rwrite() dispatches on the
/// code to pick the client stream versus the log file).
pub fn emit_info_coded(flag: InfoFlag, level: u8, code: LogCode, message: String) {
    EVENTS.with(|e| {
        e.borrow_mut().push(Stamped::stamp(DiagnosticEvent::Info {
            flag,
            level,
            code,
            message,
        }));
    });
}

/// Emit a warning diagnostic, unconditionally.
///
/// A warning is not gated on verbosity: upstream's `FWARNING` is a
/// first-class log code that `rwrite()` dispatches on its own
/// (`log.c:341`), and it is carried over the socket for protocols >= 30
/// (`rsync.h:285`, `rsync.h:296` `MSG_WARNING = FWARNING`). Emitting it at
/// `level` 0 keeps [`info_gte`] trivially true, so the message is produced
/// whatever the operator passed for `-v`.
///
/// The event carries [`LogCode::Warning`], which the stream router sends to
/// stderr (see `stream.rs`). The
/// `rsync warning: … (code N) at file(line)` trailer upstream renders at
/// `log.c:956` is deliberately not applied here; that formatting belongs to
/// the single logcode-to-stream funnel, not to the emitter.
// upstream: log.c:341 rwrite() `case FWARNING:`
pub fn emit_warning(message: String) {
    emit_info_coded(InfoFlag::Misc, 0, LogCode::Warning, message);
}

/// Emit a debug diagnostic event into the thread-local buffer.
///
/// The caller is responsible for checking [`debug_gte`] first; the
/// [`debug_log!`](crate::debug_log) macro handles this automatically.
///
/// The event is tagged [`LogCode::Info`]: upstream debug traces are emitted
/// through `rprintf(FINFO, ...)` and share FINFO routing. Producers that
/// mirror a different upstream code use [`emit_debug_coded`].
// upstream: log.c:rwrite() dispatches FINFO debug messages
pub fn emit_debug(flag: DebugFlag, level: u8, message: String) {
    emit_debug_coded(flag, level, LogCode::Info, message);
}

/// Emit a debug diagnostic event carrying an explicit upstream [`LogCode`].
///
/// Use this when the producing site mirrors an upstream `rprintf(code, ...)`
/// whose code is not `FINFO` (upstream: log.c:rwrite() dispatches on the
/// code to pick the client stream versus the log file).
pub fn emit_debug_coded(flag: DebugFlag, level: u8, code: LogCode, message: String) {
    EVENTS.with(|e| {
        e.borrow_mut().push(Stamped::stamp(DiagnosticEvent::Debug {
            flag,
            level,
            code,
            message,
        }));
    });
}

/// Drain all collected events with their production order, clearing the buffer.
///
/// This is the drain for a consumer that has to interleave these events with
/// another output channel: the key says which of two messages was produced
/// first, which the buffers alone cannot say once they are drained at
/// different times.
pub fn drain_stamped_events() -> Vec<Stamped<DiagnosticEvent>> {
    EVENTS.with(|e| e.borrow_mut().drain(..).collect())
}

/// Drain all collected events, clearing the internal buffer.
///
/// Returns events in emission order. After draining, the buffer is empty
/// and ready to accumulate new events.
///
/// A projection of [`drain_stamped_events`] for the consumers that render this
/// buffer on its own: with nothing to interleave against, the vector's own
/// order already is the production order, and the key would be noise.
pub fn drain_events() -> Vec<DiagnosticEvent> {
    drain_stamped_events()
        .into_iter()
        .map(Stamped::into_value)
        .collect()
}

/// Drain the events whose code `accept` selects, leaving the rest queued.
///
/// The buffer has several independent consumers - a log-file sink, a
/// client-stream renderer, a server's peer framer - and each must take only
/// its own codes without disturbing the others. Keeping one retain loop here
/// means a new consumer supplies a policy, not another copy of the traversal.
/// Returns matches in emission order.
fn drain_stamped_events_where(accept: impl Fn(LogCode) -> bool) -> Vec<Stamped<DiagnosticEvent>> {
    EVENTS.with(|e| {
        let mut events = e.borrow_mut();
        let mut matched = Vec::new();
        events.retain(|stamped| {
            if accept(stamped.value().code()) {
                matched.push(stamped.clone());
                false
            } else {
                true
            }
        });
        matched
    })
}

/// A projection of [`drain_stamped_events_where`] for the consumers that render
/// their share of the buffer on its own, where the key would be noise.
fn drain_events_where(accept: impl Fn(LogCode) -> bool) -> Vec<DiagnosticEvent> {
    drain_stamped_events_where(accept)
        .into_iter()
        .map(Stamped::into_value)
        .collect()
}

/// Drain the events bound for the client stream, leaving `FLOG` queued.
///
/// This is the client renderer's drain, and it carries the production key
/// because that renderer has something to interleave against: the buffered
/// per-file event stream, which it emits post-hoc. `FLOG` is left behind
/// because upstream's `rwrite()` writes a log message and returns before the
/// client-stream dispatch (upstream: log.c:290-307) - the log-file sink takes
/// those through [`drain_events_coded`], and draining them here would consume
/// them before that sink ever runs.
pub fn drain_stamped_events_for_client() -> Vec<Stamped<DiagnosticEvent>> {
    drain_stamped_events_where(|code| code != LogCode::Log)
}

/// Drain only the events carrying `code`, leaving all other events queued.
///
/// This is how a log-file sink consumes `FLOG`-classified events without
/// disturbing the client-stream events that a later [`drain_events`] renders:
/// upstream's `rwrite()` writes an FLOG message to the log file and returns
/// before the client-stream dispatch (upstream: log.c:290-307), so the two
/// destinations drain independently. Returns matches in emission order.
pub fn drain_events_coded(code: LogCode) -> Vec<DiagnosticEvent> {
    drain_events_where(|candidate| candidate == code)
}

/// Drain the events a server must frame to its peer, leaving the rest queued.
///
/// A server does not print these itself: upstream's `rwrite()` hands them to
/// `send_msg()` and returns (upstream: log.c:357-366), so the message travels
/// as a `MSG_*` frame and the client renders it. Without this drain the events
/// stay in the buffer and die with the thread, which is why a daemon receiver
/// could degrade to unconfined path syscalls without the operator ever seeing
/// the warning that says so.
///
/// Takes exactly the codes upstream frames *unconditionally*:
///
/// - `FERROR_XFER`, `FERROR`, `FWARNING` - reach the stream switch and set
///   `f` (upstream: log.c:336-342), so the `am_server` block always runs.
/// - `FERROR_SOCKET` and `FERROR_UTF8` normalise to `FERROR` first
///   (upstream: log.c:303-308), so they belong to the same class.
///
/// `FINFO` is deliberately excluded even though upstream does frame it. Its
/// arm returns early under `quiet` (upstream: log.c:344-345), so forwarding it
/// correctly needs the caller's quiet state, and `LogCode::Info` is also what
/// [`emit_debug`] tags every debug trace with - draining it here would put the
/// server's whole `--debug` output on the wire as a side effect of fixing a
/// warning. `FLOG` is excluded because upstream returns before the switch
/// (upstream: log.c:331-333); it belongs to [`drain_events_coded`].
// upstream: log.c:357-366 rwrite() `if (am_server ...) send_msg(msg, ...)`
pub fn drain_events_for_peer() -> Vec<DiagnosticEvent> {
    drain_events_where(|code| {
        matches!(
            code,
            LogCode::ErrorXfer
                | LogCode::Error
                | LogCode::Warning
                | LogCode::ErrorSocket
                | LogCode::ErrorUtf8
        )
    })
}

/// Apply an info flag token to the current thread's configuration.
///
/// Parses tokens like `"copy2"` or `"del"` and updates the corresponding
/// flag level. This supports the `--info=FLAGS` CLI option
/// (upstream: options.c:parse_output_words()).
pub fn apply_info_flag(token: &str) -> Result<(), String> {
    VERBOSITY.with(|v| v.borrow_mut().apply_info_flag(token))
}

/// Apply a debug flag token to the current thread's configuration.
///
/// Parses tokens like `"recv2"` or `"flist"` and updates the corresponding
/// flag level. This supports the `--debug=FLAGS` CLI option
/// (upstream: options.c:parse_output_words()).
pub fn apply_debug_flag(token: &str) -> Result<(), String> {
    VERBOSITY.with(|v| v.borrow_mut().apply_debug_flag(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_and_check() {
        let mut config = VerbosityConfig::default();
        config.info.copy = 2;
        config.debug.recv = 3;

        init(config);

        assert!(info_gte(InfoFlag::Copy, 1));
        assert!(info_gte(InfoFlag::Copy, 2));
        assert!(!info_gte(InfoFlag::Copy, 3));

        assert!(debug_gte(DebugFlag::Recv, 1));
        assert!(debug_gte(DebugFlag::Recv, 3));
        assert!(!debug_gte(DebugFlag::Recv, 4));
    }

    #[test]
    fn test_emit_and_drain() {
        init(VerbosityConfig::default());

        emit_info(InfoFlag::Copy, 1, "test info".to_owned());
        emit_debug(DebugFlag::Recv, 2, "test debug".to_owned());

        let events = drain_events();
        assert_eq!(events.len(), 2);

        match &events[0] {
            DiagnosticEvent::Info {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(*flag, InfoFlag::Copy);
                assert_eq!(*level, 1);
                assert_eq!(message, "test info");
            }
            _ => panic!("expected info event"),
        }

        match &events[1] {
            DiagnosticEvent::Debug {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(*flag, DebugFlag::Recv);
                assert_eq!(*level, 2);
                assert_eq!(message, "test debug");
            }
            _ => panic!("expected debug event"),
        }

        assert_eq!(drain_events().len(), 0);
    }

    /// The behaviour-preserving default: every event emitted through the
    /// legacy emitters (and therefore through `info_log!`/`debug_log!`) is
    /// tagged FINFO, the code whose upstream routing - client stdout,
    /// subject to verbosity (upstream: log.c:317-320) - matches how the
    /// renderer already treats all drained events.
    #[test]
    fn default_emitters_tag_events_finfo() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info(InfoFlag::Copy, 1, "info default".to_owned());
        emit_debug(DebugFlag::Recv, 1, "debug default".to_owned());

        let events = drain_events();
        assert_eq!(events.len(), 2);
        for event in &events {
            assert_eq!(event.code(), LogCode::Info);
        }
    }

    /// Coded emitters attach the producer's real upstream logcode, e.g. FLOG
    /// for log-file-only banners (upstream: log.c:290-307).
    #[test]
    fn coded_emitters_carry_explicit_logcode() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info_coded(
            InfoFlag::Flist,
            1,
            LogCode::Log,
            "building file list".to_owned(),
        );
        emit_debug_coded(
            DebugFlag::Recv,
            1,
            LogCode::Client,
            "client-only".to_owned(),
        );

        let events = drain_events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].code(), LogCode::Log);
        assert_eq!(events[1].code(), LogCode::Client);
        assert!(matches!(
            &events[0],
            DiagnosticEvent::Info { code: LogCode::Log, message, .. } if message == "building file list"
        ));
    }

    #[test]
    fn diagnostic_event_clone() {
        let info_event = DiagnosticEvent::Info {
            flag: InfoFlag::Copy,
            level: 1,
            code: LogCode::Info,
            message: "test".to_owned(),
        };
        let cloned = info_event;
        match cloned {
            DiagnosticEvent::Info {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(flag, InfoFlag::Copy);
                assert_eq!(level, 1);
                assert_eq!(message, "test");
            }
            _ => panic!("expected info event"),
        }

        let debug_event = DiagnosticEvent::Debug {
            flag: DebugFlag::Bind,
            level: 2,
            code: LogCode::Info,
            message: "debug".to_owned(),
        };
        let cloned = debug_event;
        match cloned {
            DiagnosticEvent::Debug {
                flag,
                level,
                message,
                ..
            } => {
                assert_eq!(flag, DebugFlag::Bind);
                assert_eq!(level, 2);
                assert_eq!(message, "debug");
            }
            _ => panic!("expected debug event"),
        }
    }

    #[test]
    fn diagnostic_event_debug() {
        let info_event = DiagnosticEvent::Info {
            flag: InfoFlag::Del,
            level: 3,
            code: LogCode::Info,
            message: "delete".to_owned(),
        };
        let debug = format!("{info_event:?}");
        assert!(debug.contains("Info"));
        assert!(debug.contains("Del"));

        let debug_event = DiagnosticEvent::Debug {
            flag: DebugFlag::Io,
            level: 4,
            code: LogCode::Info,
            message: "io debug".to_owned(),
        };
        let debug = format!("{debug_event:?}");
        assert!(debug.contains("Debug"));
        assert!(debug.contains("Io"));
    }

    #[test]
    fn apply_info_flag_valid_token() {
        init(VerbosityConfig::default());
        let result = apply_info_flag("copy2");
        assert!(result.is_ok());
        assert!(info_gte(InfoFlag::Copy, 2));
    }

    #[test]
    fn apply_info_flag_invalid_token() {
        init(VerbosityConfig::default());
        let result = apply_info_flag("invalid_flag");
        assert!(result.is_err());
    }

    #[test]
    fn apply_debug_flag_valid_token() {
        init(VerbosityConfig::default());
        let result = apply_debug_flag("io3");
        assert!(result.is_ok());
        assert!(debug_gte(DebugFlag::Io, 3));
    }

    #[test]
    fn apply_debug_flag_invalid_token() {
        init(VerbosityConfig::default());
        let result = apply_debug_flag("not_a_flag");
        assert!(result.is_err());
    }

    #[test]
    fn info_gte_default_config() {
        init(VerbosityConfig::default());
        assert!(!info_gte(InfoFlag::Backup, 1));
        assert!(info_gte(InfoFlag::Backup, 0));
    }

    #[test]
    fn debug_gte_default_config() {
        init(VerbosityConfig::default());
        assert!(!debug_gte(DebugFlag::Acl, 1));
        assert!(debug_gte(DebugFlag::Acl, 0));
    }

    #[test]
    fn oc_accelerated_io_debug_flags_gated_off_by_default() {
        init(VerbosityConfig::default());
        drain_events();

        // Default config leaves oc-specific categories at level 0, so a
        // gated debug_log! must be a no-op and emit nothing.
        for flag in [
            DebugFlag::Iouring,
            DebugFlag::Clone,
            DebugFlag::Sockopt,
            DebugFlag::Iocp,
        ] {
            assert!(!debug_gte(flag, 1));
        }
        crate::debug_log!(Iouring, 1, "should not emit");
        crate::debug_log!(Clone, 1, "should not emit");
        crate::debug_log!(Sockopt, 1, "should not emit");
        crate::debug_log!(Iocp, 1, "should not emit");
        assert!(drain_events().is_empty());

        // Enabling the category makes the same call emit exactly one event.
        let mut config = VerbosityConfig::default();
        config.debug.clone = 1;
        init(config);
        crate::debug_log!(Clone, 1, "CoW clone succeeded");
        assert_eq!(drain_events().len(), 1);
    }

    #[test]
    fn multiple_events_ordering() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info(InfoFlag::Copy, 1, "first".to_owned());
        emit_info(InfoFlag::Del, 2, "second".to_owned());
        emit_debug(DebugFlag::Send, 3, "third".to_owned());

        let events = drain_events();
        assert_eq!(events.len(), 3);

        match &events[0] {
            DiagnosticEvent::Info { message, .. } => assert_eq!(message, "first"),
            _ => panic!("expected info"),
        }
        match &events[1] {
            DiagnosticEvent::Info { message, .. } => assert_eq!(message, "second"),
            _ => panic!("expected info"),
        }
        match &events[2] {
            DiagnosticEvent::Debug { message, .. } => assert_eq!(message, "third"),
            _ => panic!("expected debug"),
        }
    }

    #[test]
    fn drain_events_clears_buffer() {
        init(VerbosityConfig::default());
        drain_events();

        emit_info(InfoFlag::Copy, 1, "test".to_owned());
        let first_drain = drain_events();
        assert_eq!(first_drain.len(), 1);

        let second_drain = drain_events();
        assert_eq!(second_drain.len(), 0);
    }

    #[test]
    fn reinit_overwrites_config() {
        let mut config1 = VerbosityConfig::default();
        config1.info.copy = 5;
        init(config1);
        assert!(info_gte(InfoFlag::Copy, 5));

        let config2 = VerbosityConfig::default();
        init(config2);
        assert!(!info_gte(InfoFlag::Copy, 1));
    }
}
