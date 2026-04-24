//! Tests verifying multiplex activation depends on protocol version.
//!
//! Upstream rsync activates multiplex output at different thresholds depending
//! on the execution context:
//! - Server mode: protocol >= 23 (main.c:1247-1248)
//! - Client mode: protocol >= 30 (need_messages_from_generator, compat.c:776)
//!
//! These tests exercise `requires_multiplex_output` for each supported protocol
//! version in both server and client mode.

use protocol::ProtocolVersion;

use crate::requires_multiplex_output;
use crate::role::ServerRole;

// -- Server mode: multiplex enabled for all supported versions (>= 28 > 23) --

#[test]
fn server_receiver_v28_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Receiver,
        ProtocolVersion::V28,
        None,
    ));
}

#[test]
fn server_receiver_v29_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Receiver,
        ProtocolVersion::V29,
        None,
    ));
}

#[test]
fn server_receiver_v30_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Receiver,
        ProtocolVersion::V30,
        None,
    ));
}

#[test]
fn server_receiver_v31_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Receiver,
        ProtocolVersion::V31,
        None,
    ));
}

#[test]
fn server_receiver_v32_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Receiver,
        ProtocolVersion::V32,
        None,
    ));
}

#[test]
fn server_generator_v28_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Generator,
        ProtocolVersion::V28,
        None,
    ));
}

#[test]
fn server_generator_v32_enables_multiplex() {
    assert!(requires_multiplex_output(
        false,
        ServerRole::Generator,
        ProtocolVersion::V32,
        None,
    ));
}

// -- Client mode: multiplex enabled only for protocol >= 30 --

#[test]
fn client_sender_v28_disables_multiplex() {
    assert!(!requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V28,
        None,
    ));
}

#[test]
fn client_sender_v29_disables_multiplex() {
    assert!(!requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V29,
        None,
    ));
}

#[test]
fn client_sender_v30_enables_multiplex() {
    assert!(requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V30,
        None,
    ));
}

#[test]
fn client_sender_v31_enables_multiplex() {
    assert!(requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V31,
        None,
    ));
}

#[test]
fn client_sender_v32_enables_multiplex() {
    assert!(requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V32,
        None,
    ));
}

#[test]
fn client_receiver_v28_disables_multiplex() {
    assert!(!requires_multiplex_output(
        true,
        ServerRole::Receiver,
        ProtocolVersion::V28,
        None,
    ));
}

#[test]
fn client_receiver_v29_disables_multiplex() {
    assert!(!requires_multiplex_output(
        true,
        ServerRole::Receiver,
        ProtocolVersion::V29,
        None,
    ));
}

#[test]
fn client_receiver_v30_enables_multiplex() {
    assert!(requires_multiplex_output(
        true,
        ServerRole::Receiver,
        ProtocolVersion::V30,
        None,
    ));
}

#[test]
fn client_receiver_v31_enables_multiplex() {
    assert!(requires_multiplex_output(
        true,
        ServerRole::Receiver,
        ProtocolVersion::V31,
        None,
    ));
}

#[test]
fn client_receiver_v32_enables_multiplex() {
    assert!(requires_multiplex_output(
        true,
        ServerRole::Receiver,
        ProtocolVersion::V32,
        None,
    ));
}

// -- Edge case: compat_flags parameter does not affect the decision --

#[test]
fn compat_flags_do_not_affect_multiplex_decision() {
    use protocol::CompatibilityFlags;

    let flags = Some(
        CompatibilityFlags::INC_RECURSE
            | CompatibilityFlags::SYMLINK_TIMES
            | CompatibilityFlags::SYMLINK_ICONV,
    );

    // Server mode with flags - still enabled (protocol >= 23)
    assert!(requires_multiplex_output(
        false,
        ServerRole::Receiver,
        ProtocolVersion::V30,
        flags,
    ));

    // Client mode v29 with flags - still disabled (protocol < 30)
    assert!(!requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V29,
        flags,
    ));

    // Client mode v30 with flags - still enabled (protocol >= 30)
    assert!(requires_multiplex_output(
        true,
        ServerRole::Generator,
        ProtocolVersion::V30,
        flags,
    ));
}
