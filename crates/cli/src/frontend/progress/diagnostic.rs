//! Rendering of info and debug diagnostic events.
//!
//! This module provides infrastructure for rendering diagnostic messages
//! from the logging system's thread-local event queue.

use std::io::{self, Write};

pub use logging::DiagnosticEvent;

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
                flag,
                level: _,
                message,
            } => {
                // Debug always goes to stderr with flag prefix
                writeln!(err, "[{flag:?}] {message}")?;
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

    #[test]
    fn test_debug_event_renders_to_stderr_with_flag() {
        let events = vec![DiagnosticEvent::Debug {
            flag: DebugFlag::Filter,
            level: 1,
            message: "excluding file foo.txt".to_owned(),
        }];

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        render_diagnostic_events(&events, &mut stdout, &mut stderr, false).unwrap();

        assert!(stdout.is_empty());
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "[Filter] excluding file foo.txt\n"
        );
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
        let stderr_output = String::from_utf8(stderr).unwrap();
        assert!(stderr_output.contains("info message\n"));
        assert!(stderr_output.contains("[Filter] debug message\n"));
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

        let stdout_output = String::from_utf8(stdout).unwrap();
        assert!(stdout_output.contains("first\n"));
        assert!(stdout_output.contains("third\n"));

        let stderr_output = String::from_utf8(stderr).unwrap();
        assert_eq!(stderr_output, "[Io] second\n");
    }

    #[test]
    fn test_flush_diagnostics_drains_events() {
        // Emit some events to the thread-local queue
        logging::emit_info(InfoFlag::Progress, 1, "test info".to_owned());
        logging::emit_debug(DebugFlag::Filter, 1, "test debug".to_owned());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        // Flush should drain and render them
        flush_diagnostics(&mut stdout, &mut stderr, false).unwrap();

        let stdout_output = String::from_utf8(stdout).unwrap();
        assert!(stdout_output.contains("test info"));

        let stderr_output = String::from_utf8(stderr).unwrap();
        assert!(stderr_output.contains("[Filter] test debug"));

        // Second flush should be empty
        let mut stdout2 = Vec::new();
        let mut stderr2 = Vec::new();
        flush_diagnostics(&mut stdout2, &mut stderr2, false).unwrap();
        assert!(stdout2.is_empty());
        assert!(stderr2.is_empty());
    }
}
