//! Which receivers forward a peer's log frames instead of rendering them.
//!
//! upstream: main.c:1085 `send_msgs_to_gen = am_server` - every server
//! receiver child passes a peer `MSG_INFO`/`MSG_ERROR` to its generator
//! (log.c:292-301), whose `rwrite()` frames it to the client (log.c:357-366).

use std::ffi::OsString;
use std::path::PathBuf;

use protocol::ProtocolVersion;

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::reader::LocalRole;
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

/// WHY: a daemon receiver is a server; its peer's `MSG_INFO` must reach the
/// client and the daemon log, never the receiver's own stdout.
#[test]
fn daemon_receiver_forwards_peer_log_frames() {
    assert_eq!(
        receiver(false, true, Some("/srv/mod")).reader_role(),
        LocalRole::Server
    );
}

/// WHY: an rsh `--server` receiver's stdout IS the protocol stream, so a
/// peer's `MSG_INFO` rendered there would interleave raw bytes with frames.
/// upstream: main.c:1085 `send_msgs_to_gen = am_server`.
#[test]
fn ssh_server_receiver_forwards_peer_log_frames() {
    assert_eq!(
        receiver(false, false, None).reader_role(),
        LocalRole::Server
    );
}

#[test]
fn daemon_client_receiver_renders_peer_log_frames_locally() {
    // upstream: a pulling client is not `am_server`, even though oc marks both
    // ends of an rsync:// transfer as a daemon connection.
    assert_eq!(receiver(true, true, None).reader_role(), LocalRole::Client);
}
