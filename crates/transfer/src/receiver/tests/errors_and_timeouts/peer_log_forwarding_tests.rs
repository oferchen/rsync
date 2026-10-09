//! Which receivers route a peer's log frames to the daemon's log channel.
//!
//! upstream: log.c:292-301 - every receiver child passes a peer `MSG_INFO`/
//! `MSG_ERROR` to its generator; log.c:312-330 - only a daemon's generator
//! writes it to the log file.

use std::ffi::OsString;
use std::path::PathBuf;

use protocol::ProtocolVersion;

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

fn receiver(
    client_mode: bool,
    daemon_connection: bool,
    module_root: Option<&str>,
) -> ReceiverContext {
    let handshake = HandshakeResult {
        protocol: ProtocolVersion::try_from(32).unwrap(),
        buffered: Vec::new(),
        compat_exchanged: false,
        client_args: None,
        io_timeout: None,
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    };
    let mut config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.connection.client_mode = client_mode;
    config.connection.is_daemon_connection = daemon_connection;
    config.connection.daemon_module_root = module_root.map(PathBuf::from);
    ReceiverContext::new_for_test(&handshake, config)
}

#[test]
fn daemon_receiver_forwards_peer_log_frames() {
    assert!(receiver(false, true, Some("/srv/mod")).forwards_peer_log());
}

#[test]
fn ssh_server_receiver_renders_peer_log_frames_locally() {
    assert!(!receiver(false, false, None).forwards_peer_log());
}

#[test]
fn daemon_client_receiver_renders_peer_log_frames_locally() {
    // upstream: a pulling client is not `am_daemon`, even though oc marks both
    // ends of an rsync:// transfer as a daemon connection.
    assert!(!receiver(true, true, None).forwards_peer_log());
}
