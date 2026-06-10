//! Receiver-side `munge symlinks` regression tests.
//!
//! Mirrors the on-disk transform upstream applies in `flist.c:1122-1126`:
//! when the daemon module has `munge symlinks = yes`, every symlink that the
//! receiver materializes carries the `/rsyncd-munged/` prefix so that
//! following the link cannot escape the module root. The complementary
//! sender-side strip lives in `super::munge_symlinks` (file-list entry).
//!
//! # Upstream Reference
//!
//! - `clientserver.c:992-1004` - daemon resolves `munge_symlinks` from
//!   `lp_munge_symlinks()` and aborts if `rsyncd-munged` already exists at
//!   the module root.
//! - `flist.c:1122-1126` - receiver prepends `SYMLINK_PREFIX` to the wire
//!   target before the link is written to disk.

use std::ffi::OsString;
use std::io::{self, Write};

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;
use crate::writer::MsgInfoSender;

/// Sink that captures emitted MSG_INFO frames so the test can assert
/// itemize output without touching the daemon multiplex layer.
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

fn munge_receiver_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        munge_symlinks: true,
        ..Default::default()
    }
}

fn plain_receiver_config() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    }
}

#[test]
fn receiver_prepends_munge_prefix_to_on_disk_symlink() {
    // upstream: flist.c:1122-1126 - the receiver-side prepend is the only
    // signal that the daemon enabled `munge symlinks`. Verify the on-disk
    // link carries the `/rsyncd-munged/` prefix so following it lands inside
    // the module root.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, munge_receiver_config());
    ctx.file_list = vec![FileEntry::new_symlink(
        "escape".into(),
        "/etc/passwd".into(),
    )];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_symlinks(dest, None, &mut writer);

    let on_disk = std::fs::read_link(dest.join("escape")).expect("read_link");
    assert_eq!(
        on_disk,
        std::path::Path::new("/rsyncd-munged//etc/passwd"),
        "receiver must prepend `/rsyncd-munged/` so following the link \
         cannot escape the module root (upstream flist.c:1122-1126)",
    );
}

#[test]
fn receiver_writes_unmunged_target_when_disabled() {
    // Negative control: the same flist with `munge_symlinks=false` must
    // produce a byte-identical target on disk. The munge transform is
    // strictly opt-in via daemon configuration.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, plain_receiver_config());
    ctx.file_list = vec![FileEntry::new_symlink(
        "escape".into(),
        "/etc/passwd".into(),
    )];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_symlinks(dest, None, &mut writer);

    let on_disk = std::fs::read_link(dest.join("escape")).expect("read_link");
    assert_eq!(
        on_disk,
        std::path::Path::new("/etc/passwd"),
        "without `munge symlinks`, the receiver writes the wire target verbatim",
    );
}
