//! Symlink and special-file surface: itemize emission for files,
//! directories, and symlinks, plus the `MsgInfoSender` plumbing that
//! receivers use to surface those events.

use std::io::{self, Write};

use protocol::flist::FileEntry;

use super::support::{test_config, test_handshake};
use crate::config::ServerConfig;
use crate::generator::ItemFlags;
use crate::writer::MsgInfoSender;

use super::super::ReceiverContext;

/// A test writer that records MSG_INFO payloads for verification.
struct MockMsgInfoWriter {
    messages: Vec<Vec<u8>>,
}

impl MockMsgInfoWriter {
    fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }
}

impl Write for MockMsgInfoWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for MockMsgInfoWriter {
    fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
        self.messages.push(data.to_vec());
        Ok(())
    }
}

fn receiver_config_with_itemize() -> ServerConfig {
    let mut config = test_config();
    config.flags.info_flags.itemize = true;
    // Server mode (not client mode) to enable emission
    config.connection.client_mode = false;
    config
}

#[test]
fn emit_itemize_new_file_transfer() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("docs/readme.txt".into(), 1024, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert_eq!(writer.messages.len(), 1);
    let msg = String::from_utf8_lossy(&writer.messages[0]);
    // Receiver uses is_sender=false, producing '>' prefix
    assert_eq!(msg, ">f+++++++++ docs/readme.txt\n");
}

#[test]
fn emit_itemize_updated_file_transfer() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("data.bin".into(), 512, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert_eq!(writer.messages.len(), 1);
    let msg = String::from_utf8_lossy(&writer.messages[0]);
    assert_eq!(msg, ">f......... data.bin\n");
}

#[test]
fn emit_itemize_directory_creation() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_directory("subdir".into(), 0o755);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert_eq!(writer.messages.len(), 1);
    let msg = String::from_utf8_lossy(&writer.messages[0]);
    assert_eq!(msg, "cd+++++++++ subdir/\n");
}

#[test]
fn emit_itemize_root_directory_emits_creation_glyph_when_iflags_zero() {
    // Regression: upstream `testsuite/itemize.test` expects `cd+++++++++ ./`
    // as the second line of `-iplr from/ to/` against a non-existent dest.
    // The root entry (path == ".") arrives at emit_itemize with iflags == 0
    // because oc-rsync's create_directory_incremental cannot observe the
    // pre-flight mkdir performed by `ensure_dest_root_exists`. Mirror
    // upstream main.c:794-796 + generator.c:566-572 by treating the root
    // dir as freshly created so the receiver still emits the line.
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_directory(".".into(), 0o755);
    let iflags = ItemFlags::from_raw(0);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert_eq!(writer.messages.len(), 1);
    let msg = String::from_utf8_lossy(&writer.messages[0]);
    assert_eq!(msg, "cd+++++++++ ./\n");
}

#[test]
fn emit_itemize_up_to_date_file() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("unchanged.txt".into(), 256, 0o644);
    // No flags - file is up-to-date, no changes
    let iflags = ItemFlags::from_raw(0);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    // upstream: generator.c:574-576 - no output when iflags has no significant
    // flags (file is completely unchanged)
    assert_eq!(writer.messages.len(), 0);
}

#[test]
fn emit_itemize_skipped_in_client_mode() {
    let handshake = test_handshake();
    let mut config = receiver_config_with_itemize();
    config.connection.client_mode = true; // Client mode suppresses emission
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert!(writer.messages.is_empty());
}

#[test]
fn emit_itemize_skipped_without_itemize_flag() {
    let handshake = test_handshake();
    let mut config = test_config();
    config.flags.info_flags.itemize = false;
    config.connection.client_mode = false;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_TRANSFER | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert!(writer.messages.is_empty());
}

#[test]
fn emit_itemize_symlink_with_target() {
    let handshake = test_handshake();
    let config = receiver_config_with_itemize();
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    let mut writer = MockMsgInfoWriter::new();

    let entry = FileEntry::new_symlink("mylink".into(), "target".into());
    let iflags = ItemFlags::from_raw(ItemFlags::ITEM_LOCAL_CHANGE | ItemFlags::ITEM_IS_NEW);

    ctx.emit_itemize(&mut writer, &iflags, &entry).unwrap();

    assert_eq!(writer.messages.len(), 1);
    let msg = String::from_utf8_lossy(&writer.messages[0]);
    assert_eq!(msg, "cL+++++++++ mylink -> target\n");
}

#[test]
fn should_emit_itemize_conditions() {
    let handshake = test_handshake();

    // Server mode + itemize -> true
    let mut config = test_config();
    config.connection.client_mode = false;
    config.flags.info_flags.itemize = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(ctx.should_emit_itemize());

    // Client mode + itemize -> false
    let mut config = test_config();
    config.connection.client_mode = true;
    config.flags.info_flags.itemize = true;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(!ctx.should_emit_itemize());

    // Server mode + no itemize -> false
    let mut config = test_config();
    config.connection.client_mode = false;
    config.flags.info_flags.itemize = false;
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(!ctx.should_emit_itemize());
}

#[test]
fn msg_info_sender_default_noop() {
    // Verify that a bare Write impl with no MsgInfoSender override
    // uses the default no-op behavior
    struct PlainWriter;
    impl Write for PlainWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl MsgInfoSender for PlainWriter {}

    let mut w = PlainWriter;
    // Default impl should succeed silently
    w.send_msg_info(b"test data").unwrap();
}
