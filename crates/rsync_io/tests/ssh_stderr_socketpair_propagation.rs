//! Integration tests for SSH stderr error-message propagation over the
//! socketpair-backed aux channel (SSE-6, #2375).
//!
//! These tests exercise the primitive landed in SSE-3
//! (`rsync_io::ssh::socketpair_stderr::make_stderr_socketpair`) by wiring
//! the child end of a `socketpair(AF_UNIX, SOCK_STREAM, 0)` into a real
//! subprocess as its stderr file descriptor, then asserting that bytes
//! written to fd 2 inside the subprocess surface byte-for-byte on the
//! parent end. The drain mirrors SSE-4's intended behaviour: each
//! complete line is appended to a bounded snapshot buffer and emitted as
//! a `tracing::warn!` event so downstream consumers can observe loss-free
//! error propagation without scraping the local process stderr.
//!
//! The async `AsyncSshTransport::stderr_capture()` accessor is staged
//! behind SSE-4 (PR #4363) and is therefore not relied on here; the
//! synthetic in-process socketpair drives the same kernel path the
//! production code takes once SSE-4 wires the drain through tokio.
//!
//! Gated on `cfg(unix)` because `socketpair(2)` is Unix-only, and on
//! `feature = "ssh-socketpair-stderr"` because the primitive module is
//! itself feature-gated.

#![cfg(all(unix, feature = "ssh-socketpair-stderr"))]

use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use rsync_io::ssh::socketpair_stderr::make_stderr_socketpair;
use tracing::dispatcher;
use tracing::{Dispatch, Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
use tracing_subscriber::registry::Registry;

/// Captures the formatted `message` field of every `WARN` event observed
/// while the subscriber is active. Shared across the drain thread and
/// the assertion thread via `Arc<Mutex<_>>`.
#[derive(Clone, Default)]
struct WarnCapture {
    messages: Arc<Mutex<Vec<String>>>,
}

impl WarnCapture {
    fn new() -> Self {
        Self::default()
    }

    fn snapshot(&self) -> Vec<String> {
        self.messages.lock().expect("warn capture mutex").clone()
    }
}

impl<S> Layer<S> for WarnCapture
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        if let Some(message) = visitor.message {
            self.messages
                .lock()
                .expect("warn capture mutex")
                .push(message);
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
}

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_owned());
        }
    }
}

/// Drains the parent half line-by-line, appending complete lines to
/// `buffer` and emitting one `tracing::warn!` per line. Mirrors the
/// shape of the production drain loop staged in SSE-4 so the assertions
/// here cover the same observable behaviour.
fn drain_lines(parent: File, buffer: Arc<Mutex<Vec<u8>>>) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(parent);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(_) => {
                buffer
                    .lock()
                    .expect("drain buffer mutex")
                    .extend_from_slice(&line);
                let text = String::from_utf8_lossy(&line);
                tracing::warn!("ssh stderr: {}", text.trim_end_matches('\n'));
            }
            Err(_) => break,
        }
    }
}

/// Spawns `argv` with `child_end` installed as fd 2 (stderr). Returns
/// the spawned `Child` so the caller can `wait()` it.
fn spawn_with_stderr_fd(argv: &[&str], child_end: File) -> std::process::Child {
    let child_fd: OwnedFd = child_end.into();
    let mut command = Command::new(argv[0]);
    command.args(&argv[1..]);
    command.stdin(Stdio::null());
    command.stdout(Stdio::null());
    command.stderr(Stdio::from(child_fd));
    command.spawn().expect("spawn subprocess")
}

/// Runs `body` with the [`WarnCapture`] layer installed as the
/// thread-local default subscriber and returns the captured warnings
/// plus whatever `body` produced. The configured [`Dispatch`] is also
/// handed to `body` so caller-spawned threads can opt in to the same
/// subscriber via [`dispatcher::with_default`] (the default dispatcher
/// is thread-local and is not inherited by `std::thread::spawn`).
fn with_warn_capture<R, F: FnOnce(Dispatch) -> R>(body: F) -> (Vec<String>, R) {
    let capture = WarnCapture::new();
    let subscriber = Registry::default().with(capture.clone());
    let dispatch = Dispatch::new(subscriber);
    let result = dispatcher::with_default(&dispatch, || body(dispatch.clone()));
    (capture.snapshot(), result)
}

/// Single-line propagation: one `echo` writes one line to stderr, the
/// drain captures the bytes verbatim and emits exactly one warning.
#[test]
fn single_error_line_propagates_through_socketpair() {
    let (parent, child_end) = make_stderr_socketpair().expect("create socketpair");
    let buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let (warnings, _) = with_warn_capture(|dispatch| {
        let mut child = spawn_with_stderr_fd(
            &["/bin/sh", "-c", "echo \"test error message\" >&2"],
            child_end,
        );
        let drain_buffer = Arc::clone(&buffer);
        let drain = std::thread::spawn(move || {
            dispatcher::with_default(&dispatch, || drain_lines(parent, drain_buffer));
        });
        let status = child.wait().expect("wait child");
        assert!(status.success(), "subprocess exited with {status}");
        drain.join().expect("drain thread");
    });

    let collected = buffer.lock().expect("buffer mutex").clone();
    assert_eq!(
        collected, b"test error message\n",
        "socketpair must surface the child's stderr bytes verbatim"
    );
    assert_eq!(
        warnings.len(),
        1,
        "exactly one tracing::warn must fire for the single error line"
    );
    assert!(
        warnings[0].contains("test error message"),
        "warning payload must carry the captured line, got {:?}",
        warnings[0]
    );
}

/// Multi-line propagation: three separate `echo` calls produce three
/// lines on stderr; the drain captures all three in order and emits
/// exactly three `tracing::warn` events with matching payloads.
#[test]
fn multi_error_lines_propagate_and_fire_one_warn_each() {
    let (parent, child_end) = make_stderr_socketpair().expect("create socketpair");
    let buffer: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let (warnings, _) = with_warn_capture(|dispatch| {
        let mut child = spawn_with_stderr_fd(
            &[
                "/bin/sh",
                "-c",
                "echo \"first error\" >&2; echo \"second error\" >&2; echo \"third error\" >&2",
            ],
            child_end,
        );
        let drain_buffer = Arc::clone(&buffer);
        let drain = std::thread::spawn(move || {
            dispatcher::with_default(&dispatch, || drain_lines(parent, drain_buffer));
        });
        let status = child.wait().expect("wait child");
        assert!(status.success(), "subprocess exited with {status}");
        drain.join().expect("drain thread");
    });

    let collected = buffer.lock().expect("buffer mutex").clone();
    assert_eq!(
        collected, b"first error\nsecond error\nthird error\n",
        "all three lines must surface in order on the parent end"
    );
    assert_eq!(
        warnings.len(),
        3,
        "one tracing::warn must fire per stderr line, got {warnings:?}"
    );
    assert!(warnings[0].contains("first error"));
    assert!(warnings[1].contains("second error"));
    assert!(warnings[2].contains("third error"));
}

/// EOF is observed cleanly once the subprocess exits and the child end
/// of the socketpair is closed. Confirms the drain returns without
/// blocking past child reap - the failure mode the design doc lists
/// under section 8 ("Tokio drain task survives past child reap").
#[test]
fn drain_returns_after_child_exit_closes_child_end() {
    let (parent, child_end) = make_stderr_socketpair().expect("create socketpair");
    // No `Arc` indirection needed; this test only asserts the drain
    // thread completes without help from a deadline timer.
    let mut child = spawn_with_stderr_fd(&["/bin/sh", "-c", "echo \"goodbye\" >&2"], child_end);
    let drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut parent = parent;
        parent.read_to_end(&mut buf).expect("read parent end");
        buf
    });
    let status = child.wait().expect("wait child");
    assert!(status.success(), "subprocess exited with {status}");
    let collected = drain.join().expect("drain thread");
    assert_eq!(collected, b"goodbye\n");
}
