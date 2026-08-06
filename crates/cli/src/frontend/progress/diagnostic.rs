//! Rendering of info and debug diagnostic events.
//!
//! This module provides infrastructure for rendering diagnostic messages
//! from the logging system's thread-local event queue.

use std::cell::Cell;
use std::io::{self, Write};

pub use logging::DiagnosticEvent;

thread_local! {
    // The workflow records its resolved `--msgs-to-stderr` decision here so
    // the post-execute final flush in `frontend::mod` can route any leftover
    // diagnostic events to the same stream the workflow used. Defaults to
    // false (upstream rsync's FINFO default: stdout in client mode).
    // upstream: log.c:rwrite() routes FINFO to client stdout unless
    // `msgs2stderr` is set, so initial events that emit before the workflow
    // records its decision still land on the correct stream.
    static MSGS_TO_STDERR: Cell<bool> = const { Cell::new(false) };
}

/// Record the effective `--msgs-to-stderr` setting for the current thread.
///
/// The workflow calls this once it has resolved the CLI flag (and the
/// `RSYNC_OUTPUT_TARGET=All` mode override) so that any diagnostic events
/// left in the thread-local queue when execution returns are routed to the
/// same stream the workflow itself used.
pub fn set_msgs_to_stderr(value: bool) {
    MSGS_TO_STDERR.with(|cell| cell.set(value));
}

/// Read the recorded `--msgs-to-stderr` setting for the current thread.
pub fn msgs_to_stderr() -> bool {
    MSGS_TO_STDERR.with(|cell| cell.get())
}

/// The client-side destination `rwrite()` picks for a given log code.
///
/// upstream: log.c:251-328 `rwrite()`. The four outcomes are the stdout
/// default (`FILE *f = ... : stdout`, log.c:254), the stderr override for the
/// error and warning codes (log.c:309-316), the silent return taken by an
/// FLOG message with no log destination (log.c:306-307), and the fatal
/// "Bad logcode" default arm (log.c:325-327).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClientStream {
    /// The client's stdout, unless `--msgs-to-stderr` moves it (log.c:254).
    Stdout,
    /// The client's stderr, whatever `--msgs-to-stderr` says (log.c:315).
    Stderr,
    /// Never reaches the client at all (log.c:306-307).
    Dropped,
    /// Not a code `rwrite()` accepts; upstream reports it and exits
    /// `RERR_MESSAGEIO` (log.c:325-327).
    Invalid,
}

/// Classify a [`logging::LogCode`] the way upstream's `rwrite()` does.
///
/// The match is exhaustive on purpose: a log code added to the vocabulary
/// later must be classified here rather than inheriting stdout by default.
// upstream: log.c:281-328 rwrite()
const fn client_stream(code: logging::LogCode) -> ClientStream {
    use logging::LogCode;

    match code {
        // FERROR_SOCKET is simplified to FERROR and FERROR_UTF8 becomes
        // FERROR before the switch (log.c:281-286), so all five error and
        // warning codes reach `f = stderr` (log.c:310-316).
        LogCode::ErrorXfer
        | LogCode::Error
        | LogCode::Warning
        | LogCode::ErrorSocket
        | LogCode::ErrorUtf8 => ClientStream::Stderr,
        // FCLIENT is rewritten to FINFO (log.c:288-289) and FINFO keeps the
        // stdout default (log.c:317-320).
        LogCode::Info | LogCode::Client => ClientStream::Stdout,
        // An FLOG message goes to the log file when one is active and is
        // otherwise discarded; it never reaches the client's stdout/stderr
        // (log.c:290-307). The log-file sink consumes FLOG events via
        // `logging::drain_events_coded` before this renderer runs, so any
        // FLOG event still queued here has no log destination.
        LogCode::Log => ClientStream::Dropped,
        // FNONE is "never sent" (rsync.h:276) and falls into rwrite()'s
        // default arm (log.c:325-327).
        LogCode::None => ClientStream::Invalid,
    }
}

/// Render diagnostic events to the stream their log code selects.
///
/// Error and warning codes go to stderr, info codes to stdout, and FLOG
/// events are dropped, mirroring `rwrite()`. When `msgs2stderr` is true the
/// stdout default becomes stderr, which upstream does by initialising `f` to
/// stderr before the code switch (upstream: log.c:254).
///
/// # Arguments
///
/// * `events` - The diagnostic events to render.
/// * `out` - The stdout writer (used for info codes when `msgs2stderr` is false).
/// * `err` - The stderr writer.
/// * `msgs2stderr` - Whether to route the stdout-bound messages to stderr too.
///
/// # Errors
///
/// Returns an I/O error if writing to either stream fails, or if an event
/// carries a code `rwrite()` rejects (upstream: log.c:325-327 exits
/// `RERR_MESSAGEIO`).
pub fn render_diagnostic_events<O: Write, E: Write>(
    events: &[DiagnosticEvent],
    out: &mut O,
    err: &mut E,
    msgs2stderr: bool,
) -> io::Result<()> {
    for event in events {
        let code = event.code();
        let (DiagnosticEvent::Info { message, .. } | DiagnosticEvent::Debug { message, .. }) =
            event;
        // upstream: rwrite() prints debug traces through rprintf(FINFO, ...)
        // with no flag-category bracket, so info and debug events differ only
        // in the code they carry, never in their rendering.
        match client_stream(code) {
            ClientStream::Stderr => writeln!(err, "{message}")?,
            ClientStream::Stdout if msgs2stderr => writeln!(err, "{message}")?,
            ClientStream::Stdout => writeln!(out, "{message}")?,
            ClientStream::Dropped => {}
            ClientStream::Invalid => {
                // upstream: log.c:325-327 reports the bad code on stderr and
                // exits RERR_MESSAGEIO. A renderer cannot exit, so it reports
                // and hands the failure back to its caller.
                writeln!(err, "Bad logcode in rwrite(): {}", code.as_u8())?;
                return Err(io::Error::other(format!(
                    "Bad logcode in rwrite(): {}",
                    code.as_u8()
                )));
            }
        }
    }
    Ok(())
}

/// Drain any pending diagnostic events from the thread-local queue and render them.
///
/// This integrates with `logging::drain_events()` to collect all pending
/// events and render them to the appropriate output streams.
///
/// # Arguments
///
/// * `out` - The stdout writer.
/// * `err` - The stderr writer.
/// * `msgs2stderr` - Whether to route all messages to stderr.
///
/// # Errors
///
/// Returns an I/O error if rendering fails.
pub fn flush_diagnostics<O: Write, E: Write>(
    out: &mut O,
    err: &mut E,
    msgs2stderr: bool,
) -> io::Result<()> {
    let events = logging::drain_events();
    if !events.is_empty() {
        render_diagnostic_events(&events, out, err, msgs2stderr)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use logging::{DebugFlag, InfoFlag, LogCode};

    #[test]
    fn test_info_event_renders_to_stdout() {
        let events = vec![DiagnosticEvent::Info {
            flag: InfoFlag::Progress,
            level: 1,
            code: LogCode::Info,
            message: "transferred 1024 bytes".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "transferred 1024 bytes\n"
        );
        assert!(stderr.is_empty());
    }

    /// upstream: log.c:rwrite - a debug message (FINFO) renders verbatim on
    /// the client's stdout, with no flag-category bracket. This is the stream
    /// the fuzzy testsuite greps for "fuzzy basis selected ...".
    #[test]
    fn test_debug_event_renders_to_stdout_verbatim() {
        let events = vec![DiagnosticEvent::Debug {
            flag: DebugFlag::Filter,
            level: 1,
            code: LogCode::Info,
            message: "excluding file foo.txt".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            "excluding file foo.txt\n"
        );
        assert!(stderr.is_empty());
    }

    /// upstream: log.c:309-316 - FWARNING (and every error code) overrides the
    /// stdout default with `f = stderr`. stdout and stderr are distinct
    /// observables: a warning on stdout corrupts `--out-format` output for a
    /// caller that redirects the two streams separately.
    #[test]
    fn test_warning_coded_event_renders_to_stderr_not_stdout() {
        let events = vec![DiagnosticEvent::Info {
            flag: InfoFlag::Misc,
            level: 1,
            code: LogCode::Warning,
            message: "WARNING: foo.txt failed verification".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "WARNING: foo.txt failed verification\n"
        );
        assert!(stdout.is_empty(), "a warning must never reach stdout");
    }

    /// Both directions in one render call: the info-coded line must land on
    /// stdout and *only* stdout, the warning-coded line on stderr and *only*
    /// stderr. Dispatching both by a single hardcoded stream fails this test
    /// whichever stream is hardcoded.
    #[test]
    fn test_info_and_warning_split_across_streams() {
        let events = vec![
            DiagnosticEvent::Info {
                flag: InfoFlag::Name,
                level: 1,
                code: LogCode::Info,
                message: "foo.txt".to_owned(),
            },
            DiagnosticEvent::Debug {
                flag: DebugFlag::Recv,
                level: 1,
                code: LogCode::Warning,
                message: "WARNING: foo.txt failed verification".to_owned(),
            },
        ];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        assert_eq!(String::from_utf8(stdout).unwrap(), "foo.txt\n");
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "WARNING: foo.txt failed verification\n"
        );
    }

    /// upstream: log.c:281-286 simplify FERROR_SOCKET and FERROR_UTF8 to
    /// FERROR before the switch, and log.c:310-313 routes FERROR_XFER and
    /// FERROR alongside FWARNING to stderr.
    #[test]
    fn test_every_error_code_renders_to_stderr() {
        for code in [
            LogCode::ErrorXfer,
            LogCode::Error,
            LogCode::Warning,
            LogCode::ErrorSocket,
            LogCode::ErrorUtf8,
        ] {
            let events = vec![DiagnosticEvent::Info {
                flag: InfoFlag::Misc,
                level: 1,
                code,
                message: format!("{code} message"),
            }];

            let mut stdout = Vec::new();
            let mut stderr = Vec::new();

            render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

            assert_eq!(
                String::from_utf8(stderr).unwrap(),
                format!("{code} message\n")
            );
            assert!(stdout.is_empty(), "{code} must not reach stdout");
        }
    }

    /// upstream: log.c:288-289 rewrites FCLIENT to FINFO, so it keeps the
    /// stdout default rather than falling into the "Bad logcode" arm.
    #[test]
    fn test_client_coded_event_renders_to_stdout() {
        let events = vec![DiagnosticEvent::Info {
            flag: InfoFlag::Misc,
            level: 1,
            code: LogCode::Client,
            message: "client note".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        assert_eq!(String::from_utf8(stdout).unwrap(), "client note\n");
        assert!(stderr.is_empty());
    }

    /// upstream: log.c:325-327 reports an unaccepted code on stderr and exits
    /// RERR_MESSAGEIO. The renderer reports and fails instead of quietly
    /// printing the message to stdout.
    #[test]
    fn test_none_coded_event_is_rejected_loudly() {
        let events = vec![DiagnosticEvent::Info {
            flag: InfoFlag::Misc,
            level: 1,
            code: LogCode::None,
            message: "unclassified".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let error = render_diagnostic_events(&events, &mut stdout, &mut stderr, false)
            .expect_err("FNONE must not be rendered");

        assert!(error.to_string().contains("Bad logcode in rwrite()"));
        assert!(String::from_utf8(stderr).unwrap().contains("Bad logcode"));
        assert!(stdout.is_empty());
    }

    /// Pins the classification of every code in the vocabulary, so adding a
    /// variant forces a deliberate choice here as well as in the match.
    #[test]
    fn test_client_stream_classification_is_total() {
        for code in LogCode::ALL {
            let expected = match code {
                LogCode::ErrorXfer
                | LogCode::Error
                | LogCode::Warning
                | LogCode::ErrorSocket
                | LogCode::ErrorUtf8 => ClientStream::Stderr,
                LogCode::Info | LogCode::Client => ClientStream::Stdout,
                LogCode::Log => ClientStream::Dropped,
                LogCode::None => ClientStream::Invalid,
            };
            assert_eq!(client_stream(code), expected, "{code}");
        }
    }

    /// `--msgs-to-stderr` moves the stdout-bound codes only; a warning was
    /// already on stderr and stays there (upstream: log.c:254 initialises `f`
    /// to stderr, and the switch's `f = stderr` is then a no-op).
    #[test]
    fn test_msgs2stderr_does_not_move_warnings_to_stdout() {
        let events = vec![DiagnosticEvent::Info {
            flag: InfoFlag::Misc,
            level: 1,
            code: LogCode::Warning,
            message: "warned".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, true).unwrap();

        assert_eq!(String::from_utf8(stderr).unwrap(), "warned\n");
        assert!(stdout.is_empty());
    }

    #[test]
    fn test_msgs2stderr_routes_all_to_stderr() {
        let events = vec![
            DiagnosticEvent::Info {
                flag: InfoFlag::Progress,
                level: 1,
                code: LogCode::Info,
                message: "info message".to_owned(),
            },
            DiagnosticEvent::Debug {
                flag: DebugFlag::Filter,
                level: 1,
                code: LogCode::Info,
                message: "debug message".to_owned(),
            },
        ];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, true).unwrap();

        assert!(stdout.is_empty());
        // With msgs-to-stderr, both info and debug route to stderr; debug
        // renders verbatim with no flag-category bracket (upstream fidelity).
        let stderr_output = String::from_utf8(stderr).unwrap();
        assert!(stderr_output.contains("info message\n"));
        assert!(stderr_output.contains("debug message\n"));
        assert!(!stderr_output.contains("[Filter]"));
    }

    #[test]
    fn test_multiple_events_rendered_in_order() {
        let events = vec![
            DiagnosticEvent::Info {
                flag: InfoFlag::Progress,
                level: 1,
                code: LogCode::Info,
                message: "first".to_owned(),
            },
            DiagnosticEvent::Debug {
                flag: DebugFlag::Io,
                level: 2,
                code: LogCode::Info,
                message: "second".to_owned(),
            },
            DiagnosticEvent::Info {
                flag: InfoFlag::Stats,
                level: 1,
                code: LogCode::Info,
                message: "third".to_owned(),
            },
        ];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        // upstream: FINFO info AND debug messages both route to the client's
        // stdout, verbatim, with no flag-category bracket prefix
        // (log.c:rwrite via rprintf(FINFO, ...)). Order is preserved.
        let stdout_output = String::from_utf8(stdout).unwrap();
        assert_eq!(stdout_output, "first\nsecond\nthird\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn test_flush_diagnostics_drains_events() {
        logging::emit_info(InfoFlag::Progress, 1, "test info".to_owned());
        logging::emit_debug(DebugFlag::Filter, 1, "test debug".to_owned());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        flush_diagnostics(&mut stdout, &mut stderr, false).unwrap();

        // Without msgs-to-stderr, both info and debug land on stdout; debug
        // renders verbatim with no flag-category bracket (upstream fidelity).
        let stdout_output = String::from_utf8(stdout).unwrap();
        assert!(stdout_output.contains("test info"));
        assert!(stdout_output.contains("test debug"));
        assert!(!stdout_output.contains("[Filter]"));
        assert!(stderr.is_empty());

        let mut stdout2 = Vec::new();
        let mut stderr2 = Vec::new();
        flush_diagnostics(&mut stdout2, &mut stderr2, false).unwrap();
        assert!(stdout2.is_empty());
        assert!(stderr2.is_empty());
    }
}
