//! `warn_log!` must actually reach stderr, and must do so at every verbosity.
//!
//! The macro was added alongside a receiver call site that reports a daemon
//! continuing without path confinement. That call site is only useful if the
//! message reaches an operator, and "it routes the same way `debug_log!` does"
//! is an argument rather than a measurement - the kind that has already been
//! wrong twice in this crate, once because `debug_gte` short-circuits before
//! the message is ever formatted, and once because the chosen level was
//! unreachable at every verbosity an operator can set.
//!
//! So both links are asserted here rather than inferred:
//!
//! 1. `warn_log!` emits an event carrying [`LogCode::Warning`];
//! 2. `LogCode::Warning` resolves to [`MessageStream::Stderr`].
//!
//! # Upstream Reference
//!
//! - `log.c:309-316` - `FWARNING` shares the `f = stderr` arm with `FERROR`.
//! - `log.c:317-320` - only `FINFO` consults `quiet`, so a warning is not
//!   suppressible the way an info message is.
//! - `rsync.h:296` - `MSG_WARNING = FWARNING`, carried to the peer for
//!   protocols >= 30.

use logging::{
    DiagnosticEvent, LogCode, MessageStream, Msgs2Stderr, StreamContext, VerbosityConfig,
    drain_events, init, message_stream, warn_log,
};

/// Pull the single event `warn_log!` produced, failing loudly if the macro
/// emitted nothing - that silence is the exact defect this file guards.
fn emit_and_take_one(message: &str) -> (LogCode, u8, String) {
    let mut events = drain_events();
    assert_eq!(
        events.len(),
        1,
        "warn_log! must emit exactly one event, got {}: {events:?}",
        events.len()
    );
    match events.pop().expect("length checked above") {
        DiagnosticEvent::Info {
            code,
            level,
            message: emitted,
            ..
        } => {
            assert_eq!(emitted, message, "the formatted message must survive");
            (code, level, emitted)
        }
        other => panic!("warn_log! must emit an Info-shaped event, got {other:?}"),
    }
}

/// Link 1: the macro emits `Warning`, at a level that cannot gate it out.
#[test]
fn warn_log_emits_a_warning_coded_event() {
    init(VerbosityConfig::default());
    drain_events();

    warn_log!("planted {} warning", "test");
    let (code, level, _) = emit_and_take_one("planted test warning");

    assert_eq!(
        code,
        LogCode::Warning,
        "the code is what routes the message; anything else lands elsewhere"
    );
    assert_eq!(
        level, 0,
        "level 0 keeps the info gate trivially true - a higher level would \
         make the message unreachable at operator-settable verbosities, which \
         is the bug this macro exists to avoid"
    );
}

/// Link 2: `Warning` routes to stderr, and keeps doing so under every
/// `StreamContext` an operator can produce - including `--quiet`, which
/// suppresses only `FINFO` upstream.
#[test]
fn warning_routes_to_stderr_under_every_context() {
    for quiet in [false, true] {
        for msgs2stderr in [
            Msgs2Stderr::Never,
            Msgs2Stderr::Always,
            Msgs2Stderr::Default,
        ] {
            for log_destination in [false, true] {
                let ctx = StreamContext {
                    quiet,
                    msgs2stderr,
                    log_destination,
                };
                assert_eq!(
                    message_stream(LogCode::Warning, ctx),
                    Ok(MessageStream::Stderr),
                    "a warning must reach stderr for {ctx:?}"
                );
            }
        }
    }
}

/// The control that makes the test above mean something: the same matrix over
/// `Info` does NOT always reach stderr. Without this, both assertions would
/// pass on a build where `message_stream` returned `Stderr` unconditionally.
#[test]
fn info_is_not_unconditionally_stderr() {
    let quiet_ctx = StreamContext {
        quiet: true,
        ..StreamContext::default()
    };
    assert_eq!(
        message_stream(LogCode::Info, quiet_ctx),
        Ok(MessageStream::Suppressed),
        "control failed: if Info is not suppressed under --quiet, the router \
         is not discriminating and the warning assertions prove nothing"
    );

    let plain_ctx = StreamContext::default();
    assert_eq!(
        message_stream(LogCode::Info, plain_ctx),
        Ok(MessageStream::Stdout),
        "control failed: Info must default to stdout, so 'Warning -> Stderr' \
         is a real routing decision rather than the only outcome"
    );
}
