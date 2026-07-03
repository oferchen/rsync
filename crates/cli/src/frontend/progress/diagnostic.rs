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

/// Render diagnostic events to the appropriate output stream.
///
/// When `msgs2stderr` is true, all diagnostic output goes to stderr.
/// Otherwise, info messages go to stdout and debug messages go to stderr.
///
/// # Arguments
///
/// * `events` - The diagnostic events to render.
/// * `out` - The stdout writer (used for info messages when `msgs2stderr` is false).
/// * `err` - The stderr writer (used for debug messages and all messages when `msgs2stderr` is true).
/// * `msgs2stderr` - Whether to route all messages to stderr.
///
/// # Errors
///
/// Returns an I/O error if writing to either stream fails.
pub fn render_diagnostic_events<O: Write, E: Write>(
    events: &[DiagnosticEvent],
    out: &mut O,
    err: &mut E,
    msgs2stderr: bool,
) -> io::Result<()> {
    for event in events {
        match event {
            DiagnosticEvent::Info {
                flag: _,
                level: _,
                message,
            } => {
                if msgs2stderr {
                    writeln!(err, "{message}")?;
                } else {
                    writeln!(out, "{message}")?;
                }
            }
            DiagnosticEvent::Debug {
                flag: _,
                level: _,
                message,
            } => {
                // upstream: log.c:rwrite() prints debug messages verbatim via
                // rprintf(FINFO, ...) with no flag-category bracket. FINFO
                // routes to the client's stdout (the same stream as info
                // events) unless msgs-to-stderr is in effect, so debug lines
                // like "fuzzy basis selected ..." land on stdout where the
                // fuzzy testsuite greps them. The message already carries any
                // role prefix (e.g. "[sender]") where upstream emits one.
                if msgs2stderr {
                    writeln!(err, "{message}")?;
                } else {
                    writeln!(out, "{message}")?;
                }
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
    use logging::{DebugFlag, InfoFlag};

    #[test]
    fn test_info_event_renders_to_stdout() {
        let events = vec![DiagnosticEvent::Info {
            flag: InfoFlag::Progress,
            level: 1,
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

    #[test]
    fn test_msgs2stderr_routes_all_to_stderr() {
        let events = vec![
            DiagnosticEvent::Info {
                flag: InfoFlag::Progress,
                level: 1,
                message: "info message".to_owned(),
            },
            DiagnosticEvent::Debug {
                flag: DebugFlag::Filter,
                level: 1,
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
                message: "first".to_owned(),
            },
            DiagnosticEvent::Debug {
                flag: DebugFlag::Io,
                level: 2,
                message: "second".to_owned(),
            },
            DiagnosticEvent::Info {
                flag: InfoFlag::Stats,
                level: 1,
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
