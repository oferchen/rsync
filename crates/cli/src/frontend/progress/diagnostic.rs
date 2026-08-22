//! Rendering of info and debug diagnostic events.
//!
//! This module provides infrastructure for rendering diagnostic messages
//! from the logging system's thread-local event queue.

use std::cell::Cell;
use std::io::{self, Write};

pub use logging::DiagnosticEvent;
use logging::Stamped;

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
/// The context every client-side stream decision is taken in.
///
/// upstream: log.c:251 `rwrite()` is the sole chooser of a stream. The decision
/// lives in `logging::message_stream` so that this renderer and every other
/// diagnostic emitter share one implementation of the rule; a second copy here
/// is what made #352 and #357 two separate bugs instead of one. This helper
/// keeps the *inputs* to that rule equally singular.
fn stream_context(msgs2stderr: bool) -> logging::StreamContext {
    logging::StreamContext {
        // upstream: log.c:345 `if (quiet)` - the FINFO arm of rwrite() returns
        // without writing when quiet is set. Folding `--quiet` into
        // `verbose = 0` at parse time silences only the verbosity-gated
        // events; a notice upstream prints at DEFAULT verbosity - `skipping
        // directory %s` (flist.c:1338), `skipping non-regular file "%s"` under
        // `INFO_GTE(NONREG, 1)` - still reaches this renderer and has to be
        // suppressed here, where upstream suppresses it.
        quiet: logging::finfo_suppressed(),
        // `Never` and `Default` select the same stream (log.c:253 keys on
        // `== 1`), so a caller that only knows the boolean loses nothing here.
        // The tri-state is available for callers that have it.
        msgs2stderr: if msgs2stderr {
            logging::Msgs2Stderr::Always
        } else {
            logging::Msgs2Stderr::Default
        },
        log_destination: false,
    }
}

/// The stream the transfer-summary writer represents.
///
/// `with_output_writer` hands `emit_transfer_summary` a single writer chosen by
/// the same `--msgs-to-stderr` decision `message_stream` applies, so a
/// diagnostic routed to this stream is one the summary renderer can interleave;
/// anything else has to be written by the caller, which holds both handles.
const fn summary_stream(msgs2stderr: bool) -> logging::MessageStream {
    if msgs2stderr {
        logging::MessageStream::Stderr
    } else {
        logging::MessageStream::Stdout
    }
}

/// Splits drained diagnostics by whether they belong on the summary's stream.
///
/// The first half keeps its production key so [`PendingDiagnostics`] can
/// interleave it with the buffered event stream at the position upstream would
/// have written it. The second half is handed back to
/// [`render_diagnostic_events`], which re-derives the stream per event - so the
/// routing rule stays in `logging::message_stream` alone, and an event carrying
/// a code that rule rejects stays in the second half where its diagnosis is
/// still reported rather than being silently interleaved or dropped.
///
/// [`PendingDiagnostics`]: super::PendingDiagnostics
#[must_use]
pub fn partition_by_summary_stream(
    events: Vec<Stamped<DiagnosticEvent>>,
    msgs2stderr: bool,
) -> (Vec<Stamped<String>>, Vec<DiagnosticEvent>) {
    let summary = summary_stream(msgs2stderr);
    let ctx = stream_context(msgs2stderr);
    let mut interleaved = Vec::new();
    let mut deferred = Vec::new();
    for stamped in events {
        let key = stamped.sequence();
        let event = stamped.into_value();
        if logging::message_stream(event.code(), ctx) == Ok(summary) {
            let (DiagnosticEvent::Info { message, .. } | DiagnosticEvent::Debug { message, .. }) =
                event;
            interleaved.push(Stamped::with_sequence(key, message));
        } else {
            deferred.push(event);
        }
    }
    (interleaved, deferred)
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
        match logging::message_stream(code, stream_context(msgs2stderr)) {
            Ok(logging::MessageStream::Stderr) => writeln!(err, "{message}")?,
            Ok(logging::MessageStream::Stdout) => writeln!(out, "{message}")?,
            Ok(logging::MessageStream::LogOnly | logging::MessageStream::Suppressed) => {}
            Err(_) => {
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

    /// This renderer must DELEGATE the stream choice, never re-derive it.
    ///
    /// The classification itself is pinned in `logging::stream` - including
    /// across the `--quiet` / `--msgs2stderr` cross-product, which a
    /// leaf-crate copy never covered. What is asserted here is the property
    /// that copy violated: for every code, where the renderer actually puts
    /// the bytes agrees with what the shared funnel decided. Re-deriving the
    /// rule here, however faithfully, fails this.
    #[test]
    fn test_renderer_delegates_every_code_to_the_shared_funnel() {
        for code in LogCode::ALL {
            for msgs2stderr in [false, true] {
                for quiet in [false, true] {
                    logging::set_quiet(quiet);
                    let ctx = logging::StreamContext {
                        quiet,
                        msgs2stderr: if msgs2stderr {
                            logging::Msgs2Stderr::Always
                        } else {
                            logging::Msgs2Stderr::Default
                        },
                        log_destination: false,
                    };
                    let events = vec![DiagnosticEvent::Info {
                        flag: InfoFlag::Misc,
                        level: 1,
                        code,
                        message: "probe".to_owned(),
                    }];
                    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
                    let result =
                        render_diagnostic_events(&events, &mut stdout, &mut stderr, msgs2stderr);

                    match logging::message_stream(code, ctx) {
                        Ok(logging::MessageStream::Stdout) => {
                            assert!(result.is_ok(), "{code} must render");
                            assert!(
                                String::from_utf8_lossy(&stdout).contains("probe"),
                                "{code} belongs on stdout with msgs2stderr={msgs2stderr}",
                            );
                            assert!(stderr.is_empty(), "{code} must not also hit stderr");
                        }
                        Ok(logging::MessageStream::Stderr) => {
                            assert!(result.is_ok(), "{code} must render");
                            assert!(
                                String::from_utf8_lossy(&stderr).contains("probe"),
                                "{code} belongs on stderr with msgs2stderr={msgs2stderr}",
                            );
                            assert!(stdout.is_empty(), "{code} must not also hit stdout");
                        }
                        Ok(
                            logging::MessageStream::LogOnly | logging::MessageStream::Suppressed,
                        ) => {
                            assert!(result.is_ok(), "{code} must render");
                            assert!(
                                stdout.is_empty() && stderr.is_empty(),
                                "{code} must emit nothing"
                            );
                        }
                        Err(_) => {
                            assert!(result.is_err(), "{code} must be rejected, not rendered");
                            assert!(stdout.is_empty(), "a rejected code must not reach stdout");
                        }
                    }
                }
            }
        }
        logging::set_quiet(false);
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
