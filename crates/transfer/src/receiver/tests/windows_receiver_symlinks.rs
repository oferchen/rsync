//! Windows regression tests for `ReceiverContext::create_symlinks`.
//!
//! A remote transfer to a Windows receiver must materialize symbolic links
//! from the file list instead of silently dropping them. Directory links are
//! created as a real directory symlink, falling back to a junction when the
//! process lacks the create-symlink privilege; file links have no privilege-
//! free equivalent, so a privilege refusal skips the entry with a warning and a
//! soft error (exit `RERR_PARTIAL`, 23) rather than aborting or panicking. This
//! mirrors the local-copy executor's `create_symlink` behaviour and pins it so
//! a future change cannot regress the Windows receiver back to the old no-op.
//!
//! # Upstream Reference
//!
//! - `generator.c:1544` - `if (preserve_links && ftype == FT_SYMLINK)`
//! - `generator.c:1591` - `atomic_create(file, fname, sl, ...)`

#![cfg(windows)]

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

/// Mirrors a receiver invoked as `rsync -a`: `--links` is set so symlink
/// entries in the file list are materialized.
fn links_receiver_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            links: true,
            ..Default::default()
        },
        args: vec![std::ffi::OsString::from(".")],
        ..Default::default()
    }
}

/// A directory-symlink entry resolves to its target's contents on Windows.
///
/// The receiver creates a real directory symbolic link when privileged
/// (Administrator or Developer Mode) and a junction otherwise; either way the
/// on-disk reparse point resolves to the target directory. An absolute target
/// is used so the junction fallback's `canonicalize` resolves correctly on a
/// stock unprivileged CI runner (matching `fast_io::win_symlink`'s own test).
#[test]
fn windows_receiver_symlink_materializes_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    // Real target directory with a file inside so we can read through the link.
    let target_dir = dest.join("realdir");
    std::fs::create_dir(&target_dir).expect("create target dir");
    std::fs::write(target_dir.join("inside.txt"), b"hello").expect("write inside");

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, links_receiver_config());
    ctx.file_list = vec![FileEntry::new_symlink("link".into(), target_dir.clone())];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_symlinks(dest, &mut writer)
        .expect("create_symlinks must materialize a directory link on Windows");

    let link = dest.join("link");
    let meta = std::fs::symlink_metadata(&link).expect("symlink_metadata on the created link");
    assert!(
        meta.file_type().is_symlink(),
        "a directory link must be a reparse point (symlink or junction), got {:?}",
        meta.file_type(),
    );

    let through = std::fs::read_to_string(link.join("inside.txt"))
        .expect("read target file through the link");
    assert_eq!(
        through, "hello",
        "the receiver-created directory link must resolve to the target's contents",
    );
}

/// A file-symlink entry never panics: it is created when the process holds the
/// symlink privilege, or skipped with a warning and a soft error when it does
/// not. Either outcome returns `Ok(())` so the transfer still finishes; the
/// soft error drives the `RERR_PARTIAL` (23) exit upstream produces for a
/// failed `do_symlink()`.
#[test]
fn windows_receiver_symlink_skips_file_on_privilege_refusal() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    // Real target file so the link is classified as a file link (not a dir).
    let target_file = dest.join("payload.txt");
    std::fs::write(&target_file, b"payload").expect("write target file");

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, links_receiver_config());
    ctx.file_list = vec![FileEntry::new_symlink("flink".into(), target_file.clone())];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_symlinks(dest, &mut writer)
        .expect("a file-symlink privilege refusal must skip gracefully, not error or panic");

    let link = dest.join("flink");
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        // Privileged / Developer-Mode runner: the link was created.
        assert!(
            meta.file_type().is_symlink(),
            "when created, a file link must be a symbolic link, got {:?}",
            meta.file_type(),
        );
    } else {
        // Unprivileged runner: the entry was skipped, leaving nothing behind.
        assert!(
            !link.exists(),
            "a refused file link must leave no partial entry on disk",
        );
    }
}
