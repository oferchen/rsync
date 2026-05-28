//! Receiver input multiplex activation by mode and protocol version.
//!
//! upstream: main.c:1342-1343 - client receiver activates at protocol >= 23
//! upstream: main.c:1167-1168 - server receiver activates at protocol >= 30

use std::ffi::OsString;

use protocol::ProtocolVersion;

use super::super::super::ReceiverContext;
use crate::config::ServerConfig;
use crate::handshake::HandshakeResult;
use crate::role::ServerRole;

fn receiver_with_client_mode(protocol_version: u8, client_mode: bool) -> ReceiverContext {
    let handshake = HandshakeResult {
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
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
        protocol: ProtocolVersion::try_from(protocol_version).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    config.connection.client_mode = client_mode;
    ReceiverContext::new_for_test(&handshake, config)
}

#[test]
fn client_mode_protocol_28_activates_input_multiplex() {
    // upstream: main.c:1342-1343 - protocol >= 23 activates
    let ctx = receiver_with_client_mode(28, true);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn client_mode_protocol_29_activates_input_multiplex() {
    let ctx = receiver_with_client_mode(29, true);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn client_mode_protocol_30_activates_input_multiplex() {
    let ctx = receiver_with_client_mode(30, true);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn client_mode_protocol_32_activates_input_multiplex() {
    let ctx = receiver_with_client_mode(32, true);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn server_mode_protocol_28_does_not_activate_input_multiplex() {
    // upstream: main.c:1167-1168 - server only activates for >= 30
    let ctx = receiver_with_client_mode(28, false);
    assert!(!ctx.should_activate_input_multiplex());
}

#[test]
fn server_mode_protocol_29_does_not_activate_input_multiplex() {
    let ctx = receiver_with_client_mode(29, false);
    assert!(!ctx.should_activate_input_multiplex());
}

#[test]
fn server_mode_protocol_30_activates_input_multiplex() {
    let ctx = receiver_with_client_mode(30, false);
    assert!(ctx.should_activate_input_multiplex());
}

#[test]
fn server_mode_protocol_32_activates_input_multiplex() {
    let ctx = receiver_with_client_mode(32, false);
    assert!(ctx.should_activate_input_multiplex());
}
