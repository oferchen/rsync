//! A daemon receiver writes each diagnostic to its module log before framing
//! it for the client.
//!
//! upstream: log.c:312-332 - `rwrite()` hands a daemon's message to `logit()`,
//! which writes and flushes it (log.c:135-137), before the `am_server` branch
//! frames it for the peer (log.c:357-366). A client that has read the frame can
//! therefore rely on the line already being in the daemon log.

use std::io;
use std::sync::{Arc, Mutex};

use super::super::ReceiverContext;
use super::support::{test_config, test_handshake};
use crate::progress::{DaemonFileLog, DaemonLogRow, SharedDaemonLog};
use crate::writer::MsgInfoSender;

type Lines = Arc<Mutex<Vec<Vec<u8>>>>;

struct RecordingLog(Lines);

impl DaemonFileLog for RecordingLog {
    fn on_entry(&self, _row: &DaemonLogRow) {}

    fn on_message(&self, line: &[u8]) {
        self.0.lock().unwrap().push(line.to_vec());
    }
}

/// Captures each frame together with the daemon-log lines present when it was
/// sent.
struct FrameProbe {
    log: Lines,
    frames: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
}

impl FrameProbe {
    fn record(&mut self, data: &[u8]) -> io::Result<()> {
        let logged = self.log.lock().unwrap().clone();
        self.frames.push((data.to_vec(), logged));
        Ok(())
    }
}

impl MsgInfoSender for FrameProbe {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.record(data)
    }

    fn send_msg_error_xfer(&mut self, data: &[u8]) -> io::Result<()> {
        self.record(data)
    }

    fn send_msg_error(&mut self, data: &[u8]) -> io::Result<()> {
        self.record(data)
    }

    fn send_msg_warning(&mut self, data: &[u8]) -> io::Result<()> {
        self.record(data)
    }
}

#[test]
fn every_server_diagnostic_is_logged_before_its_frame() {
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    let lines = Lines::default();
    ctx.set_daemon_log_sink(SharedDaemonLog::new(RecordingLog(Arc::clone(&lines))));
    let mut probe = FrameProbe {
        log: Arc::clone(&lines),
        frames: Vec::new(),
    };

    ctx.emit_info_line(&mut probe, "info line\n").unwrap();
    ctx.emit_warning_line(&mut probe, "warning line\n").unwrap();
    ctx.emit_error_line(&mut probe, "error line\n").unwrap();
    ctx.emit_error_xfer_line(&mut probe, b"xfer line\n")
        .unwrap();

    let expected = ["info line", "warning line", "error line", "xfer line"];
    assert_eq!(probe.frames.len(), expected.len());
    for (sent, (frame, logged)) in probe.frames.iter().enumerate() {
        let body = expected[sent].as_bytes();
        assert_eq!(
            frame.strip_suffix(b"\n"),
            Some(body),
            "frame keeps its newline"
        );
        assert_eq!(
            logged.last().map(Vec::as_slice),
            Some(body),
            "{:?} must be in the daemon log before its frame is sent",
            expected[sent]
        );
        assert_eq!(logged.len(), sent + 1, "one log line per diagnostic");
    }
}

#[test]
fn a_receiver_without_a_daemon_log_only_frames() {
    let ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    let mut probe = FrameProbe {
        log: Lines::default(),
        frames: Vec::new(),
    };

    ctx.emit_warning_line(&mut probe, "warning line\n").unwrap();

    assert_eq!(probe.frames.len(), 1);
    assert!(probe.frames[0].1.is_empty());
}
