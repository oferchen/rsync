//! `ReceiverContext::receive_id_lists` gating: ensures uid/gid lists are
//! consumed only when the matching server flag is set, and that
//! `numeric_ids` short-circuits both lists.

use std::io::Cursor;

use super::super::super::ReceiverContext;
use super::super::support::{config_with_flags, test_handshake};

#[test]
fn receive_id_lists_skips_when_numeric_ids_true() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, true);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // With numeric_ids=true, no data should be read even with owner/group set
    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    // Cursor position unchanged - nothing read
    assert_eq!(cursor.position(), 0);
}

#[test]
fn receive_id_lists_reads_uid_list_when_owner_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, false, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty UID list: varint 0 terminator only
    let data: &[u8] = &[0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 1);
}

#[test]
fn receive_id_lists_reads_gid_list_when_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, true, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Empty GID list: varint 0 terminator only
    let data: &[u8] = &[0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 1);
}

#[test]
fn receive_id_lists_reads_both_when_owner_and_group_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    // Both lists: two varint 0 terminators
    let data: &[u8] = &[0, 0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 2);
}

#[test]
fn receive_id_lists_skips_both_when_neither_flag_set() {
    let handshake = test_handshake();
    let config = config_with_flags(false, false, false);
    let mut ctx = ReceiverContext::new(&handshake, config);

    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 0);
}
