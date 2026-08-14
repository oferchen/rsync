//! Receiver-side symlink-target sanitization when munging is disabled.
//!
//! A daemon module that sets `munge symlinks = false` gives up the
//! `/rsyncd-munged/` prefix, but upstream does **not** leave the received
//! target untouched: `sanitize_paths` is on for every daemon module, so the
//! decode path strips the leading `../` that would let the stored link
//! resolve outside the module root. The two transforms are mutually
//! exclusive and together they leave no configuration in which a
//! client-supplied target reaches the disk verbatim.
//!
//! Measured 2026-08-14 against real daemons, `use chroot = false`,
//! `read only = false`, `munge symlinks = false`, client pushing
//! `escape -> ../outside`:
//!
//! ```text
//! rsync 3.4.4   module/escape -> outside       (sanitized)
//! rsync 3.5.0   module/escape -> outside       (sanitized)
//! oc-rsync      module/escape -> ../outside    (verbatim)  <- this defect
//! ```
//!
//! # Upstream Reference
//!
//! - `flist.c:1182` - `if (sanitize_paths && !munge_symlinks && *bp)
//!   sanitize_path(bp, bp, "", lastdir_depth, SP_DEFAULT)`
//! - `clientserver.c:994-995` - `if (module_dirlen) sanitize_paths = 1`, so
//!   the guard is live for every daemon module.
//! - `generator.c:1559` - the `--safe-links` check reads `F_SYMLINK(file)`,
//!   i.e. the *already* sanitized target, which is why the transform has to
//!   happen before the safety evaluation rather than at creation time.

use std::ffi::OsString;
use std::io::{self, Write};

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::super::ReceiverContext;
use super::support::test_handshake;
use crate::config::{ConnectionConfig, ServerConfig};
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;
use crate::writer::MsgInfoSender;

struct NullMsgInfoWriter;

impl Write for NullMsgInfoWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl MsgInfoSender for NullMsgInfoWriter {
    fn send_msg_info(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

/// A daemon receiver with `munge symlinks = false` - the configuration in
/// which upstream falls back to `sanitize_path`.
fn daemon_receiver_without_munging() -> ServerConfig {
    ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        munge_symlinks: false,
        connection: ConnectionConfig {
            is_daemon_connection: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Runs one symlink through the receiver and reports the on-disk target, or
/// `None` when no link was created (the `--safe-links` skip path).
fn create_one_symlink(config: ServerConfig, target: &str) -> Option<std::path::PathBuf> {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().to_path_buf();

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = vec![FileEntry::new_symlink("escape".into(), target.into())];

    let mut writer = NullMsgInfoWriter;
    ctx.create_symlinks(&dest, None, &mut writer)
        .expect("create_symlinks must succeed on a writable tempdir");

    let on_disk = std::fs::read_link(dest.join("escape")).ok();
    drop(tmp);
    on_disk
}

#[test]
fn daemon_receiver_sanitizes_an_escaping_symlink_target_when_munging_is_off() {
    // upstream: flist.c:1182 - with munging off, `sanitize_paths` still
    // strips the leading `../`, so the stored link cannot resolve above the
    // module root. Without this, a client holding only write access plants
    // `escape -> ../outside` and reaches outside the module on the next
    // transfer through that name.
    let on_disk = create_one_symlink(daemon_receiver_without_munging(), "../outside");

    assert_eq!(
        on_disk.as_deref(),
        Some(std::path::Path::new("outside")),
        "a daemon receiver with `munge symlinks = false` must sanitize the \
         received target (upstream flist.c:1182); storing `../outside` \
         verbatim lets the link resolve outside the module root",
    );
}

#[test]
fn a_non_daemon_receiver_leaves_the_target_alone() {
    // Negative control for the gate. Upstream keys the transform on
    // `sanitize_paths`, which `clientserver.c:994-995` sets only when serving
    // a daemon module - an ordinary local or SSH receiver must keep the
    // target byte-for-byte. Without this, the fix would silently rewrite
    // legitimate `../` targets on every non-daemon transfer.
    let mut config = daemon_receiver_without_munging();
    config.connection.is_daemon_connection = false;

    let on_disk = create_one_symlink(config, "../outside");

    assert_eq!(
        on_disk.as_deref(),
        Some(std::path::Path::new("../outside")),
        "only a daemon receiver has `sanitize_paths` set upstream \
         (clientserver.c:994-995); a non-daemon receiver must store the \
         target verbatim",
    );
}

#[test]
fn munging_still_wins_when_it_is_enabled() {
    // The two transforms are mutually exclusive upstream (`flist.c:1182` is
    // guarded by `!munge_symlinks`). With munging on, the target must carry
    // the prefix and must NOT have been sanitized first - sanitizing as well
    // would strip the `../` that the munge prefix already neutralises, and
    // silently change what the sender's strip round-trips back to.
    let mut config = daemon_receiver_without_munging();
    config.munge_symlinks = true;

    let on_disk = create_one_symlink(config, "../outside");

    assert_eq!(
        on_disk.as_deref(),
        Some(std::path::Path::new("/rsyncd-munged/../outside")),
        "munging and sanitization are mutually exclusive (flist.c:1182 is \
         gated on !munge_symlinks); with munging on the target keeps its \
         `../` behind the prefix",
    );
}

#[test]
fn a_non_utf8_target_is_sanitized_byte_exactly() {
    // The target is a raw byte string on the wire. Routing it through
    // `String` would replace the non-UTF-8 bytes and rewrite the value the
    // sanitizer exists to make safe, so the transform stays on `&[u8]`.
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    let tmp = tempfile::tempdir().expect("tempdir");
    let dest = tmp.path().to_path_buf();
    let target = std::path::PathBuf::from(OsString::from_vec(b"../\xFFoutside".to_vec()));

    let handshake = test_handshake();
    let mut ctx = ReceiverContext::new_for_test(&handshake, daemon_receiver_without_munging());
    ctx.file_list = vec![FileEntry::new_symlink("escape".into(), target)];

    let mut writer = NullMsgInfoWriter;
    ctx.create_symlinks(&dest, None, &mut writer)
        .expect("create_symlinks must succeed on a writable tempdir");

    let on_disk = std::fs::read_link(dest.join("escape")).expect("read_link");
    assert_eq!(
        on_disk.as_os_str().as_bytes(),
        b"\xFFoutside",
        "the traversal prefix must be stripped while every other byte \
         survives verbatim; a lossy decode would corrupt the name and defeat \
         the purpose of sanitizing it",
    );
}

#[test]
fn sanitization_precedes_the_safe_links_check() {
    // Placement, not just presence. Upstream sanitizes during the file-list
    // decode (`flist.c:1182`), so by the time the generator evaluates
    // `--safe-links` it reads the *sanitized* target
    // (`generator.c:1559` passes `F_SYMLINK(file)`). A target of
    // `../outside` therefore becomes `outside`, which is in-tree, and the
    // link is created.
    //
    // Applying the transform after the safety evaluation instead would make
    // oc skip the entry with "skipping unsafe symlink" where upstream
    // creates it - trading this defect for a different divergence, so the
    // ordering is pinned here rather than left to the next reader.
    let mut config = daemon_receiver_without_munging();
    config.flags.safe_links = true;

    let on_disk = create_one_symlink(config, "../outside");

    assert_eq!(
        on_disk.as_deref(),
        Some(std::path::Path::new("outside")),
        "sanitization must run before the --safe-links evaluation so the \
         check sees the same in-tree target upstream sees \
         (flist.c:1182 decode, then generator.c:1559); sanitizing later \
         would skip an entry upstream creates",
    );
}
