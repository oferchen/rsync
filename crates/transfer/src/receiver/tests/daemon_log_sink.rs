//! A daemon receiver writes each diagnostic to its module log before framing
//! it for the client, and its per-file rows in file-list order.
//!
//! upstream: log.c:312-332 - `rwrite()` hands a daemon's message to `logit()`,
//! which writes and flushes it (log.c:135-137), before the `am_server` branch
//! frames it for the peer (log.c:357-366). A client that has read the frame can
//! therefore rely on the line already being in the daemon log.

use std::io;
use std::path::{Path, PathBuf};
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

type Rows = Arc<Mutex<Vec<DaemonLogRow>>>;

struct RowLog(Rows);

impl DaemonFileLog for RowLog {
    fn on_entry(&self, row: &DaemonLogRow) {
        self.0.lock().unwrap().push(row.clone());
    }

    fn on_message(&self, _line: &[u8]) {}
}

fn row(name: &str, deleted: bool) -> DaemonLogRow {
    DaemonLogRow {
        name: PathBuf::from(name),
        dir: PathBuf::new(),
        size: 0,
        itemize: String::new(),
        mode: 0o100_644,
        mtime: 0,
        uid: 0,
        gid: None,
        symlink_target: None,
        hardlink_target: None,
        checksum: String::new(),
        bytes_transferred: 0,
        bytes_checksummed: 0,
        deleted,
    }
}

fn logged_names(rows: &Rows) -> Vec<(PathBuf, PathBuf)> {
    rows.lock()
        .unwrap()
        .iter()
        .map(|row| (row.dir.clone(), row.name.clone()))
        .collect()
}

/// upstream: receiver.c:919 / receiver.c:1290 - the receiver logs each entry
/// as it reads its index, so rows recorded out of order (directory creation,
/// candidate selection, commits) reach the log in file-list order, each under
/// the module-relative destination directory (log.c `case 'f'`).
#[test]
fn per_file_rows_are_written_in_file_list_order_under_the_destination_dir() {
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    let rows = Rows::default();
    ctx.set_daemon_log_sink(SharedDaemonLog::new(RowLog(Arc::clone(&rows))));
    ctx.daemon_log_dir = PathBuf::from("sub");
    {
        let mut pending = ctx.daemon_log_rows.borrow_mut();
        pending.entry(5).or_default().push(row("late", false));
        pending.entry(2).or_default().push(row("early", false));
    }
    assert!(rows.lock().unwrap().is_empty(), "rows wait for the flush");

    ctx.flush_daemon_log_rows();

    let sub = Path::new("sub").to_path_buf();
    assert_eq!(
        logged_names(&rows),
        [
            (sub.clone(), PathBuf::from("early")),
            (sub, PathBuf::from("late"))
        ]
    );
    ctx.flush_daemon_log_rows();
    assert_eq!(rows.lock().unwrap().len(), 2, "a row is written once");
}

/// upstream: log.c `log_item()` renders `f_name(file)`, so a renamed lone
/// entry logs its file-list name, while `log_delete()` names the removed path.
#[test]
fn a_renamed_entry_logs_its_file_list_name_but_a_deletion_keeps_its_own() {
    let mut ctx = ReceiverContext::new_for_test(&test_handshake(), test_config());
    let rows = Rows::default();
    ctx.set_daemon_log_sink(SharedDaemonLog::new(RowLog(Arc::clone(&rows))));
    ctx.daemon_log_renamed_from = Some(PathBuf::from("listed"));
    {
        let mut pending = ctx.daemon_log_rows.borrow_mut();
        pending.entry(0).or_default().push(row("renamed", false));
        pending.entry(1).or_default().push(row("removed", true));
    }

    ctx.flush_daemon_log_rows();

    let names: Vec<PathBuf> = logged_names(&rows).into_iter().map(|(_, n)| n).collect();
    assert_eq!(names, [PathBuf::from("listed"), PathBuf::from("removed")]);
}
