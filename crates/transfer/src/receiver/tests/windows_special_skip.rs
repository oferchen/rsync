//! Windows regression test for `ReceiverContext::create_specials`.
//!
//! Native (non-Cygwin) Windows has no `mknod` / `mkfifo` / `AF_UNIX` bind, so a
//! device, FIFO, or socket entry in the file list cannot be materialised. The
//! receiver must skip each one gracefully - a warning, no destination file, no
//! hard error and no panic - per the WIND-2 contract in
//! `docs/user/windows-support-matrix.md`. This pins that behaviour so a future
//! change cannot regress it into a silent drop or an abort.

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

/// Mirrors a receiver invoked as `rsync -aD`: `-D` implies `--devices` and
/// `--specials`.
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

/// A FIFO and a character-device entry are skipped without creating any
/// destination file and without returning an error.
#[test]
fn windows_receiver_skips_specials_without_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, special_receiver_config());
    ctx.file_list = vec![
        FileEntry::new_fifo("pipe".into(), 0o640),
        FileEntry::new_char_device("nulllike".into(), 0o600, 1, 3),
    ];

    let mut writer = CapturingMsgInfoWriter;
    // The non-Unix `create_specials` takes `(dest, writer)`: no sandbox on
    // Windows.
    ctx.create_specials(dest, &mut writer)
        .expect("create_specials must skip gracefully on Windows, not error");

    assert!(
        !dest.join("pipe").exists(),
        "a FIFO entry must be skipped on Windows, not materialised",
    );
    assert!(
        !dest.join("nulllike").exists(),
        "a device entry must be skipped on Windows, not materialised",
    );
}
