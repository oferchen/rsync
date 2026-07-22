//! Receiver-side `munge symlinks` regression tests.
//!
//! Mirrors the on-disk transform upstream applies in `flist.c:1150-1154`:
//! when the daemon module has `munge symlinks = yes`, every symlink that the
//! receiver materializes carries the `/rsyncd-munged/` prefix so that
//! following the link cannot escape the module root. The complementary
//! sender-side strip lives in `super::munge_symlinks` (file-list entry).
//!
//! # Upstream Reference
//!
//! - `clientserver.c:997-1009` - daemon resolves `munge_symlinks` from
//!   `lp_munge_symlinks()` and aborts if `rsyncd-munged` already exists at
//!   the module root.
//! - `flist.c:1150-1154` - receiver prepends `SYMLINK_PREFIX` to the wire
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
/// upstream: generator.c:1603 atomic_create -> do_symlink - EACCES is
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

/// UTS-12 regression: the receiver's `create_symlinks` must apply the
/// sender-supplied mtime to the on-disk symlink via
/// `utimensat(AT_SYMLINK_NOFOLLOW)`. Without this step every receiver-created
/// symlink wears the wall-clock time from `symlinkat(2)`, which makes upstream
/// `testsuite/alt-dest.test` fail over SSH because the `--copy-dest` checkit
/// diff catches the 1-2 second drift on `nolf-symlink`.
///
/// The local-copy path already preserves the source link's mtime via
/// `apply_symlink_metadata_with_options`. This test pins the same invariant on
/// the network receiver path (`create_symlinks`) by:
///
/// 1. Constructing a `FileEntry::new_symlink` with an explicit backdated mtime
///    well before the test run started (mirrors upstream's `nolf-symlink`
///    fixture being older than the `to/` directory is fresh).
/// 2. Driving `create_symlinks` directly so the test exercises the SSH-server
///    code path (the same call site that fires under `oc-rsync` invoked via
///    `lsh.sh`) without spinning up an SSH transport.
/// 3. Asserting `lstat` on the destination link returns the entry's mtime to
///    the exact second. Upstream's diff drops nanoseconds; matching seconds is
///    the load-bearing invariant.
///
/// upstream: generator.c:1604 `set_file_attrs(fname, file, NULL, NULL, 0)`
/// upstream: rsync.c:set_times() uses `lutimes`/`utimensat(AT_SYMLINK_NOFOLLOW)`
#[cfg(unix)]
#[test]
fn receiver_preserves_symlink_mtime_on_creation() {
    use std::os::unix::fs::MetadataExt;

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path();

    let handshake = test_handshake();
    let mut config = plain_receiver_config();
    // Mirrors what the SSH-server-side receiver sees when the client passes
    // `-ave .../lsh.sh ...`: archive mode implies `-tlogD` so `times`, `owner`,
    // and `group` are all enabled on `ParsedServerFlags`. The bug only
    // surfaces when `times` is true (without it, upstream emits no utimensat
    // syscall either).
    config.flags.times = true;
    config.flags.owner = true;
    config.flags.group = true;
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);

    // Backdated mtime: epoch + 2 hours. Pinned constants so the assertion is
    // exact and the test never races against the wall clock the way the
    // pre-fix receiver did.
    const SOURCE_MTIME_SECS: i64 = 7_200;
    let mut entry = FileEntry::new_symlink("nolf-symlink".into(), "nolf".into());
    entry.set_mtime(SOURCE_MTIME_SECS, 0);
    ctx.file_list = vec![entry];

    let mut writer = CapturingMsgInfoWriter;
    ctx.create_symlinks(dest, None, &mut writer)
        .expect("create_symlinks must succeed on a writable tempdir");

    let on_disk = std::fs::symlink_metadata(dest.join("nolf-symlink"))
        .expect("symlink_metadata must read the link itself, not its target");
    assert_eq!(
        on_disk.mtime(),
        SOURCE_MTIME_SECS,
        "receiver must propagate the entry's mtime to the on-disk symlink \
         (upstream rsync.c set_times -> utimensat(AT_SYMLINK_NOFOLLOW)); \
         without it `testsuite/alt-dest.test` diff trips on `nolf-symlink`",
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
