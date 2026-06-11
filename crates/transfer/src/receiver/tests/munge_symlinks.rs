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
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed on a writable tempdir");

    let on_disk = std::fs::read_link(dest.join("escape")).expect("read_link");
    assert_eq!(
        on_disk,
        std::path::Path::new("/rsyncd-munged//etc/passwd"),
        "receiver must prepend `/rsyncd-munged/` so following the link \
         cannot escape the module root (upstream flist.c:1122-1126)",
    );
}

/// EDG-SANDBOX.C regression: `create_symlinks` now returns
/// `io::Result<()>` and must surface a non-EACCES `symlinkat` failure
/// as `Err` instead of debug-logging and continuing with the void
/// pre-fix signature.
///
/// Before this fix, `create_symlinks` returned `()` and the
/// `Err(e) => debug_log!(...)` branch dropped every error class on the
/// floor: an ELOOP from a TOCTOU swap on a mid-path component, an
/// EOPNOTSUPP from a sandbox-anchored refusal, or an EEXIST on a
/// planted non-symlink leaf were all silently skipped while the
/// receiver exited `rc=0` with the symlink missing.
///
/// This test plants a regular file at the destination leaf so the
/// underlying `symlink` syscall returns `AlreadyExists` (EEXIST), a
/// non-EACCES class. The fix must surface it as `Err`. EACCES is
/// covered by the discrimination test in the deletion module - one
/// fail-loud regression per site is sufficient because both sites
/// share the same upstream-parity rule (`PermissionDenied` is the
/// only non-fatal class).
///
/// upstream: generator.c:1591 atomic_create -> do_symlink - EACCES is
/// non-fatal (increment io_error and continue); every other class is
/// a security boundary the receiver must surface.
#[cfg(unix)]
#[test]
fn create_symlinks_surfaces_non_eacces_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    // Plant a directory at the leaf path. `read_link` returns Err
    // (not a symlink), `lstat` sees the directory, and the obstacle
    // `unlink_via_sandbox_or_fallback(UnlinkFlags::File)` fails with
    // EISDIR which is swallowed by the pre-existing `let _ = ...`
    // obstacle-removal contract (the receiver is allowed to attempt
    // the obstacle removal but is not required to succeed). The flow
    // then reaches `symlinkat`, which returns AlreadyExists/EEXIST -
    // a non-EACCES class the fix must surface.
    std::fs::create_dir(dest.join("blocked")).expect("plant directory obstacle");

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, plain_receiver_config());
    ctx.file_list = vec![FileEntry::new_symlink(
        "blocked".into(),
        "/etc/passwd".into(),
    )];

    let mut writer = CapturingMsgInfoWriter;
    let err = ctx
        .create_symlinks(dest, None, &mut writer)
        .expect_err("non-EACCES symlinkat failure must propagate as Err, not be coerced to ()");

    assert_ne!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied,
        "EACCES is the upstream-parity non-fatal branch; this scenario \
         plants a directory at the leaf to exercise the fail-loud branch \
         via AlreadyExists/EEXIST",
    );
    // The on-disk obstacle must persist - the receiver did not silently
    // delete the directory in lieu of the symlink.
    assert!(
        dest.join("blocked").is_dir(),
        "the planted directory must persist - the receiver must not silently \
         consume it while reporting the symlink failure as `()`",
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
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed on a writable tempdir");

    let on_disk = std::fs::read_link(dest.join("escape")).expect("read_link");
    assert_eq!(
        on_disk,
        std::path::Path::new("/etc/passwd"),
        "without `munge symlinks`, the receiver writes the wire target verbatim",
    );
}
