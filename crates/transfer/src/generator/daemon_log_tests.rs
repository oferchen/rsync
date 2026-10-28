//! A daemon sender writes each diagnostic it frames for the client to its
//! module log first.
//!
//! upstream: log.c:312-332 - `rwrite()` hands a daemon's message to `logit()`,
//! which writes and flushes it (log.c:135-137), before the `am_server` branch
//! frames it for the peer (log.c:357-366).

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use crate::progress::{DaemonFileLog, DaemonLogRow, SharedDaemonLog};
use crate::writer::ServerWriter;
use crate::writer::peer_sink::PeerDiagnostics;
use protocol::ProtocolVersion;

type Lines = Arc<Mutex<Vec<Vec<u8>>>>;

struct RecordingLog(Lines);

impl DaemonFileLog for RecordingLog {
    fn on_entry(&self, _row: &DaemonLogRow) {}

    fn on_message(&self, line: &[u8]) {
        self.0.lock().unwrap().push(line.to_vec());
    }
}

/// Records, for every write that reaches the wire, how many lines the daemon
/// log already held.
struct WireProbe {
    log: Lines,
    logged_at_write: Arc<Mutex<Vec<usize>>>,
}

impl Write for WireProbe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let logged = self.log.lock().unwrap().len();
        self.logged_at_write.lock().unwrap().push(logged);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_framed_diagnostic_is_in_the_daemon_log_before_it_reaches_the_wire() {
    let _log_context = logging::enter(logging::LogContext::new());
    let lines = Lines::default();
    let _peer = PeerDiagnostics::install(
        ProtocolVersion::try_from(32u8).unwrap(),
        Some(SharedDaemonLog::new(RecordingLog(Arc::clone(&lines)))),
    );
    let logged_at_write = Arc::new(Mutex::new(Vec::new()));
    let probe = WireProbe {
        log: Arc::clone(&lines),
        logged_at_write: Arc::clone(&logged_at_write),
    };
    let mut writer = ServerWriter::new_plain(probe).activate_multiplex().unwrap();

    logging::emit_warning("sender warning".to_owned());
    writer.flush().unwrap();

    assert_eq!(*lines.lock().unwrap(), [b"sender warning".to_vec()]);
    let writes = logged_at_write.lock().unwrap();
    assert!(
        !writes.is_empty(),
        "the diagnostic must be framed for the peer"
    );
    assert!(
        writes.iter().all(|&logged| logged == 1),
        "every frame write must follow the daemon-log write: {writes:?}"
    );
}

#[test]
fn a_sender_without_a_daemon_log_still_frames_the_diagnostic() {
    let _log_context = logging::enter(logging::LogContext::new());
    let _peer = PeerDiagnostics::install(ProtocolVersion::try_from(32u8).unwrap(), None);
    let logged_at_write = Arc::new(Mutex::new(Vec::new()));
    let probe = WireProbe {
        log: Lines::default(),
        logged_at_write: Arc::clone(&logged_at_write),
    };
    let mut writer = ServerWriter::new_plain(probe).activate_multiplex().unwrap();

    logging::emit_warning("sender warning".to_owned());
    writer.flush().unwrap();

    assert!(!logged_at_write.lock().unwrap().is_empty());
}
