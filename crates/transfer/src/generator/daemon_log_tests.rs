//! A daemon sender writes each diagnostic it frames for the client to its
//! module log first.
//!
//! upstream: log.c:312-332 - `rwrite()` hands a daemon's message to `logit()`,
//! which writes and flushes it (log.c:135-137), before the `am_server` branch
//! frames it for the peer (log.c:357-366).

use std::ffi::OsString;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use super::GeneratorContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::progress::{DaemonFileLog, DaemonLogRow, SharedDaemonLog};
use crate::role::ServerRole;
use crate::writer::ServerWriter;
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

fn daemon_sender() -> GeneratorContext {
    let handshake = HandshakeResult {
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };
    let config = ServerConfig {
        role: ServerRole::Generator,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    GeneratorContext::new_for_test(&handshake, config)
}

#[test]
fn a_framed_diagnostic_is_in_the_daemon_log_before_it_reaches_the_wire() {
    let mut ctx = daemon_sender();
    let lines = Lines::default();
    ctx.set_daemon_log_sink(SharedDaemonLog::new(RecordingLog(Arc::clone(&lines))));
    let logged_at_write = Arc::new(Mutex::new(Vec::new()));
    let probe = WireProbe {
        log: Arc::clone(&lines),
        logged_at_write: Arc::clone(&logged_at_write),
    };
    let mut writer = ServerWriter::new_plain(probe).activate_multiplex().unwrap();
    let _ = logging::drain_events_for_peer();
    logged_at_write.lock().unwrap().clear();

    logging::emit_warning("sender warning".to_owned());
    ctx.flush_thread_local_diagnostics(&mut writer).unwrap();

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
    let mut ctx = daemon_sender();
    let logged_at_write = Arc::new(Mutex::new(Vec::new()));
    let probe = WireProbe {
        log: Lines::default(),
        logged_at_write: Arc::clone(&logged_at_write),
    };
    let mut writer = ServerWriter::new_plain(probe).activate_multiplex().unwrap();
    let _ = logging::drain_events_for_peer();
    logged_at_write.lock().unwrap().clear();

    logging::emit_warning("sender warning".to_owned());
    ctx.flush_thread_local_diagnostics(&mut writer).unwrap();

    assert!(!logged_at_write.lock().unwrap().is_empty());
}
