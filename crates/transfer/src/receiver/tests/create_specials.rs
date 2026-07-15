//! Regression tests for `ReceiverContext::create_specials`.
//!
//! Issue #223: the protocol wire receiver silently dropped FIFO and device
//! entries. The flist carried them (the daemon logged "built file list with N
//! entries"), but the receiver only materialised regular files, directories,
//! and symlinks, so a `rsync -a remote:src/ dst/` pull - or a push to an
//! oc-rsync daemon - lost every special with exit 0 and no error. These tests
//! pin the fix: a FIFO and a device entry in the file list must be created on
//! disk, gated on `--specials` / `--devices`, matching upstream
//! `generator.c:recv_generator`'s `FT_SPECIAL` / `FT_DEVICE` branches.

use std::io::{self, Write};

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;
use crate::writer::MsgInfoSender;

/// Sink that captures emitted MSG_INFO frames without touching the daemon
/// multiplex layer.
struct CapturingMsgInfoWriter;

impl Write for CapturingMsgInfoWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for CapturingMsgInfoWriter {
    fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

/// Builds a receiver config that mirrors what an SSH/daemon receiver sees for
/// `rsync -aD`: `-D` implies both `--devices` and `--specials`, plus the
/// archive metadata flags. Individual flags are overridden per-test.
fn special_receiver_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            devices: true,
            specials: true,
            perms: true,
            ..Default::default()
        },
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    }
}

/// upstream: generator.c:1663 - `FT_SPECIAL` entries are materialised via
/// `atomic_create -> do_mknod_at`. Without `create_specials` the receiver
/// dropped the FIFO entirely; this test fails (dest is empty) on the pre-fix
/// receiver and passes once the node is created.
#[test]
fn receiver_creates_fifo_from_flist_entry() {
    use std::os::unix::fs::FileTypeExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, special_receiver_config());
    ctx.file_list = vec![FileEntry::new_fifo("pipe".into(), 0o640)];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_specials(dest, None, &mut writer)
        .expect("create_specials must succeed on a writable tempdir");

    let meta = std::fs::symlink_metadata(dest.join("pipe"))
        .expect("the FIFO entry must be materialised, not silently dropped (#223)");
    assert!(
        meta.file_type().is_fifo(),
        "the receiver must create a FIFO node for an FT_SPECIAL flist entry",
    );
}

/// A character-device entry must be recreated with its wire rdev. Devices are
/// gated on `--devices`; upstream also requires super-user, which the metadata
/// layer's fake-super path handles when unprivileged. This test only runs when
/// the process can actually create a device node (root), degrading gracefully
/// otherwise so CI on unprivileged runners stays green.
///
/// upstream: generator.c:1627 - `FT_DEVICE` entries are created via do_mknod().
#[test]
fn receiver_creates_char_device_from_flist_entry() {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    // Pre-check privilege: attempt a throwaway mknod via the same helper the
    // receiver uses. If it fails (unprivileged, common on CI), skip the
    // assertion rather than falsely fail - the FIFO test already covers the
    // dispatch wiring on every runner.
    let probe = dest.join(".probe");
    let can_mknod =
        metadata::create_device_node_from_parts(&probe, 0o600, false, 1, 3, false).is_ok();
    let _ = std::fs::remove_file(&probe);
    if !can_mknod {
        return;
    }

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, special_receiver_config());
    ctx.file_list = vec![FileEntry::new_char_device("nulllike".into(), 0o600, 1, 3)];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_specials(dest, None, &mut writer)
        .expect("create_specials must succeed");

    let meta = std::fs::symlink_metadata(dest.join("nulllike"))
        .expect("the device entry must be materialised, not silently dropped (#223)");
    assert!(
        meta.file_type().is_char_device(),
        "the receiver must create a character device for an FT_DEVICE flist entry",
    );
    assert_eq!(
        meta.rdev(),
        metadata::device_word(1, 3),
        "the created device must carry the wire entry's rdev (major=1, minor=3)",
    );
}

/// With `--backup`, an existing destination entry must be preserved to the
/// backup location before the receiver replaces it with a fresh special node.
/// Here a regular file sits where a FIFO is about to be created: upstream's
/// `atomic_create` (generator.c:2018-2020) calls `make_backup` before removing
/// it, so the old content must survive at `pipe~` and the new FIFO must land at
/// `pipe`. Without the backup wiring the receiver would unlink the old file and
/// lose it silently.
#[test]
fn receiver_backs_up_existing_entry_before_creating_special() {
    use std::os::unix::fs::FileTypeExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    // An existing regular file occupies the path the FIFO will replace.
    std::fs::write(dest.join("pipe"), b"old-payload").expect("seed existing dest entry");

    let mut config = special_receiver_config();
    config.flags.backup = true;

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = vec![FileEntry::new_fifo("pipe".into(), 0o640)];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_specials(dest, None, &mut writer)
        .expect("create_specials must succeed");

    // The prior content is preserved under the default `~` suffix.
    assert_eq!(
        std::fs::read(dest.join("pipe~")).expect("backup must be created before replacement"),
        b"old-payload",
        "the pre-existing file must be preserved in the ~ backup, not silently unlinked",
    );
    // The fresh FIFO replaced the old entry.
    assert!(
        std::fs::symlink_metadata(dest.join("pipe"))
            .expect("the FIFO must be materialised")
            .file_type()
            .is_fifo(),
        "the receiver must create the FIFO node in place of the backed-up file",
    );
}

/// Without `--specials` the receiver must not create a FIFO - the entry is left
/// absent, matching upstream where `preserve_specials` gates FT_SPECIAL
/// creation. This guards against the fix over-creating specials.
#[test]
fn receiver_skips_fifo_without_specials_flag() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let mut config = special_receiver_config();
    config.flags.specials = false;

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = vec![FileEntry::new_fifo("pipe".into(), 0o640)];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_specials(dest, None, &mut writer)
        .expect("create_specials must succeed");

    assert!(
        std::fs::symlink_metadata(dest.join("pipe")).is_err(),
        "without --specials the receiver must not materialise the FIFO",
    );
}
