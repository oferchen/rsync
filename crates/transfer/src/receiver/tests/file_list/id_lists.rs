//! `ReceiverContext::receive_id_lists` gating: ensures uid/gid lists are
//! consumed only when the matching server flag is set, that an explicit
//! client `--numeric-ids` short-circuits both lists, and - critically - that
//! a daemon-forced numeric-ids state still reads the list off the wire.
//!
//! upstream: uidlist.c:465,473 - `recv_id_list` runs for `numeric_ids <= 0`
//! (both Off and the daemon-forced `-1`); only an explicit client
//! `--numeric-ids` (`> 0`) skips the read.

use std::io::Cursor;

use super::super::super::ReceiverContext;
use super::super::support::{config_with_flags, test_handshake};
use crate::flags::NumericIds;

#[test]
fn receive_id_lists_skips_when_numeric_ids_explicit() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, NumericIds::Explicit);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // With an explicit client --numeric-ids (upstream `> 0`), no data is read
    // even with owner/group set: the sender dropped the list from the wire.
    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    // Cursor position unchanged - nothing read
    assert_eq!(cursor.position(), 0);
}

/// Regression pin for the daemon numeric-ids wire desync: a daemon module's
/// `numeric ids = yes` maps to [`NumericIds::DaemonForced`] (upstream `-1`),
/// which suppresses local name resolution but keeps the uid/gid name-list on
/// the wire (`numeric_ids <= 0`). The receiver MUST still consume the list; a
/// prior bool collapse skipped the read and misread the list bytes as the next
/// NDX, hanging every transfer from a real upstream client (whose own
/// `numeric_ids` is `0` and therefore sends a populated list).
///
/// upstream: uidlist.c:465,473 - `(preserve_uid || preserve_acls) && numeric_ids <= 0`.
#[test]
fn receive_id_lists_reads_when_daemon_forced() {
    let handshake = test_handshake();
    let config = config_with_flags(true, true, NumericIds::DaemonForced);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Two empty lists (uid + gid): two varint 0 terminators. Under the old bool
    // behaviour these bytes would be left unread and desync the stream.
    let data: &[u8] = &[0, 0];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(
        cursor.position(),
        2,
        "daemon-forced numeric-ids must still consume both id lists"
    );
}

#[test]
fn receive_id_lists_reads_uid_list_when_owner_set() {
    let handshake = test_handshake();
    let config = config_with_flags(true, false, NumericIds::Off);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

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
    let config = config_with_flags(false, true, NumericIds::Off);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

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
    let config = config_with_flags(true, true, NumericIds::Off);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

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
    let config = config_with_flags(false, false, NumericIds::Off);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let data: &[u8] = &[];
    let mut cursor = Cursor::new(data);
    let result = ctx.receive_id_lists(&mut cursor);

    assert!(result.is_ok());
    assert_eq!(cursor.position(), 0);
}

/// Pins the upstream `recv_id_list()` remap: after reading the name list the
/// receiver rewrites every flist entry's uid from the sender's raw id to the
/// LOCAL id resolved from the transmitted name. A sender id that does not exist
/// on the receiver (but whose name does) must end up owned by the local id, not
/// the raw sender id - the whole point of the non-numeric name list.
///
/// upstream: uidlist.c:483-494 `recv_id_list` remap loop via `match_uid`.
#[test]
#[cfg(unix)]
fn remap_rewrites_flist_uid_from_sent_name() {
    use protocol::flist::FileEntry;
    use protocol::idlist::IdList;

    let handshake = test_handshake();
    // owner only (uid list), non-numeric so the name list is read + applied.
    let config = config_with_flags(true, false, NumericIds::Off);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    let proto = ctx.protocol().as_u8();

    // Sender-built uid list: a raw sender uid that does not exist locally,
    // carrying the well-known name "root" (which resolves to 0 everywhere).
    let mut sender = IdList::new();
    sender.add_id(4_000_123, Some(b"root".to_vec()));
    let mut wire = Vec::new();
    sender
        .write(&mut wire, false, proto)
        .expect("write uid list");

    let mut cursor = Cursor::new(wire);
    ctx.receive_id_lists(&mut cursor).expect("read uid list");

    // A file owned by the nonexistent sender uid.
    let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
    entry.set_uid(4_000_123);
    ctx.file_list.push(entry);

    ctx.remap_flist_ownership_from_id_lists();

    assert_eq!(
        ctx.file_list[0].uid(),
        Some(0),
        "receiver must resolve sent name 'root' to local uid 0, not keep the raw sender id"
    );
}

/// `--numeric-ids` (explicit) must leave file ownership as the raw sender id:
/// no name list is read and the remap is a no-op.
#[test]
#[cfg(unix)]
fn remap_keeps_raw_uid_under_numeric_ids() {
    use protocol::flist::FileEntry;

    let handshake = test_handshake();
    let config = config_with_flags(true, false, NumericIds::Explicit);
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    let mut entry = FileEntry::new_file("f".into(), 0, 0o644);
    entry.set_uid(4_000_123);
    ctx.file_list.push(entry);

    ctx.remap_flist_ownership_from_id_lists();

    assert_eq!(
        ctx.file_list[0].uid(),
        Some(4_000_123),
        "--numeric-ids must keep the raw sender id"
    );
}
