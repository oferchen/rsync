//! Hardlink surface: `create_hardlinks` behaviour, dry-run gating, and
//! the `HardlinkApplyTracker` lifecycle across incremental segments.

use std::ffi::OsString;

use protocol::ProtocolVersion;
use protocol::flist::FileEntry;

use super::support::{
    TestDeletionWriter, make_hlink_follower, make_hlink_leader, receiver_with_hardlinks,
    test_handshake,
};
use crate::config::ServerConfig;
use crate::flags::ParsedServerFlags;
use crate::role::ServerRole;

use super::super::ReceiverContext;

/// SEC-1.f made `create_hardlinks` take `Option<&DirSandbox>` under
/// `#[cfg(unix)]` only (DirSandbox is Unix-only). Tests run on both
/// platforms, so we route every call through this cfg-aware shim
/// rather than gating individual call sites.
fn call_create_hardlinks<W: crate::writer::MsgInfoSender + ?Sized>(
    ctx: &mut ReceiverContext,
    dest: &std::path::Path,
    writer: &mut W,
) {
    #[cfg(unix)]
    ctx.create_hardlinks(dest, None, writer)
        .expect("create_hardlinks must succeed in fixture-controlled tempdirs");
    #[cfg(not(unix))]
    ctx.create_hardlinks(dest, writer)
        .expect("create_hardlinks must succeed in fixture-controlled tempdirs");
}

#[test]
fn create_hardlinks_links_follower_to_leader() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader_file = dest.join("leader.txt");
    std::fs::write(&leader_file, "shared content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 42),
        make_hlink_follower("follower.txt", 14, 42),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    let follower_file = dest.join("follower.txt");
    assert!(follower_file.exists(), "follower should be created");
    assert_eq!(
        std::fs::read_to_string(&follower_file).unwrap(),
        "shared content"
    );
}

#[cfg(unix)]
#[test]
fn create_hardlinks_shares_inode() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader_file = dest.join("a.txt");
    std::fs::write(&leader_file, "inode check").unwrap();

    let entries = vec![
        make_hlink_leader("a.txt", 11, 100),
        make_hlink_follower("b.txt", 11, 100),
        make_hlink_follower("c.txt", 11, 100),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    let meta_a = std::fs::metadata(dest.join("a.txt")).unwrap();
    let meta_b = std::fs::metadata(dest.join("b.txt")).unwrap();
    let meta_c = std::fs::metadata(dest.join("c.txt")).unwrap();

    assert_eq!(meta_a.ino(), meta_b.ino(), "b should share inode with a");
    assert_eq!(meta_a.ino(), meta_c.ino(), "c should share inode with a");
    assert_eq!(meta_a.nlink(), 3, "nlink should be 3");
}

#[test]
fn create_hardlinks_across_directories() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::create_dir_all(dest.join("dir_a")).unwrap();
    let leader_file = dest.join("dir_a/file.txt");
    std::fs::write(&leader_file, "cross-dir").unwrap();

    let entries = vec![
        make_hlink_leader("dir_a/file.txt", 9, 50),
        make_hlink_follower("dir_b/file.txt", 9, 50),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    let follower = dest.join("dir_b/file.txt");
    assert!(
        follower.exists(),
        "follower in different dir should be created"
    );
    assert_eq!(std::fs::read_to_string(&follower).unwrap(), "cross-dir");
}

#[test]
fn create_hardlinks_multiple_groups() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("g1_leader.txt"), "group1").unwrap();
    std::fs::write(dest.join("g2_leader.txt"), "group2").unwrap();

    let entries = vec![
        make_hlink_leader("g1_leader.txt", 6, 10),
        make_hlink_follower("g1_follower.txt", 6, 10),
        make_hlink_leader("g2_leader.txt", 6, 20),
        make_hlink_follower("g2_follower.txt", 6, 20),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert_eq!(
        std::fs::read_to_string(dest.join("g1_follower.txt")).unwrap(),
        "group1"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("g2_follower.txt")).unwrap(),
        "group2"
    );
}

#[cfg(unix)]
#[test]
fn create_hardlinks_skips_already_linked() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader = dest.join("leader.txt");
    let follower = dest.join("follower.txt");
    std::fs::write(&leader, "already linked").unwrap();
    std::fs::hard_link(&leader, &follower).unwrap();

    let leader_ino = std::fs::metadata(&leader).unwrap().ino();
    let follower_ino = std::fs::metadata(&follower).unwrap().ino();
    assert_eq!(leader_ino, follower_ino);

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 77),
        make_hlink_follower("follower.txt", 14, 77),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    let meta = std::fs::metadata(&follower).unwrap();
    assert_eq!(meta.ino(), leader_ino);
}

#[test]
fn create_hardlinks_replaces_existing_file() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let leader = dest.join("leader.txt");
    let follower = dest.join("follower.txt");
    std::fs::write(&leader, "correct").unwrap();
    std::fs::write(&follower, "wrong content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 7, 88),
        make_hlink_follower("follower.txt", 7, 88),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert_eq!(std::fs::read_to_string(&follower).unwrap(), "correct");
}

#[test]
fn create_hardlinks_skipped_when_disabled() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("leader.txt"), "content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 7, 1),
        make_hlink_follower("follower.txt", 7, 1),
    ];

    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: false,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = entries;

    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert!(
        !dest.join("follower.txt").exists(),
        "follower should not be created when hard_links is disabled"
    );
}

#[test]
fn create_hardlinks_skipped_in_dry_run() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("leader.txt"), "content").unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", 7, 1),
        make_hlink_follower("follower.txt", 7, 1),
    ];

    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHnre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            dry_run: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = entries;

    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert!(
        !dest.join("follower.txt").exists(),
        "follower should not be created in dry_run mode"
    );
}

#[test]
fn create_hardlinks_follower_without_leader_is_skipped() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    let entries = vec![make_hlink_follower("orphan.txt", 10, 999)];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert!(
        !dest.join("orphan.txt").exists(),
        "orphan follower should not create a file"
    );
}

/// Verifies that the HardlinkApplyTracker is initialized when hard_links is enabled.
#[test]
fn tracker_initialized_when_hard_links_enabled() {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(
        ctx.hardlink_tracker.is_some(),
        "tracker should be initialized when hard_links is enabled"
    );
}

/// Verifies that the tracker is NOT initialized when hard_links is disabled.
#[test]
fn tracker_not_initialized_when_hard_links_disabled() {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: false,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let ctx = ReceiverContext::new_for_test(&handshake, config);
    assert!(
        ctx.hardlink_tracker.is_none(),
        "tracker should not be initialized when hard_links is disabled"
    );
}

/// Verifies that create_hardlinks populates the tracker's leader map and
/// that the tracker is restored (not consumed) after the operation.
#[test]
fn create_hardlinks_populates_tracker() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("a.txt"), "content-a").unwrap();
    std::fs::write(dest.join("b.txt"), "content-b").unwrap();

    let entries = vec![
        make_hlink_leader("a.txt", 9, 10),
        make_hlink_follower("a_link.txt", 9, 10),
        make_hlink_leader("b.txt", 9, 20),
        make_hlink_follower("b_link.txt", 9, 20),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert!(
        dest.join("a_link.txt").exists(),
        "follower a_link.txt should exist"
    );
    assert!(
        dest.join("b_link.txt").exists(),
        "follower b_link.txt should exist"
    );

    let tracker = ctx
        .hardlink_tracker
        .as_ref()
        .expect("tracker should be restored");
    assert_eq!(
        tracker.leader_count(),
        2,
        "tracker should have 2 leaders recorded"
    );
    assert_eq!(
        tracker.deferred_count(),
        0,
        "no deferred followers should remain"
    );
}

/// Verifies that the tracker correctly tracks leaders across multiple
/// create_hardlinks calls (e.g., incremental file list segments).
#[cfg(unix)]
#[test]
fn create_hardlinks_tracker_preserves_state_across_calls() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("leader.txt"), "persistent").unwrap();

    let entries_1 = vec![make_hlink_leader("leader.txt", 10, 50)];
    let mut ctx = receiver_with_hardlinks(entries_1);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    let tracker = ctx.hardlink_tracker.as_ref().unwrap();
    assert_eq!(tracker.leader_count(), 1);

    ctx.file_list = vec![
        make_hlink_leader("leader.txt", 10, 50),
        make_hlink_follower("follower.txt", 10, 50),
    ];
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    assert!(dest.join("follower.txt").exists());
    let leader_ino = std::fs::metadata(dest.join("leader.txt")).unwrap().ino();
    let follower_ino = std::fs::metadata(dest.join("follower.txt")).unwrap().ino();
    assert_eq!(
        leader_ino, follower_ino,
        "follower should share inode with leader"
    );
}

/// Verifies that three followers in the same group all share the leader's inode.
#[cfg(unix)]
#[test]
fn create_hardlinks_multiple_followers_same_group() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    std::fs::write(dest.join("original.txt"), "shared data").unwrap();

    let entries = vec![
        make_hlink_leader("original.txt", 11, 7),
        make_hlink_follower("copy1.txt", 11, 7),
        make_hlink_follower("copy2.txt", 11, 7),
        make_hlink_follower("copy3.txt", 11, 7),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;
    call_create_hardlinks(&mut ctx, dest, &mut writer);

    let leader_ino = std::fs::metadata(dest.join("original.txt")).unwrap().ino();
    for name in &["copy1.txt", "copy2.txt", "copy3.txt"] {
        let follower_ino = std::fs::metadata(dest.join(name)).unwrap().ino();
        assert_eq!(
            leader_ino, follower_ino,
            "{name} should share inode with leader"
        );
    }

    let nlink = std::fs::metadata(dest.join("original.txt"))
        .unwrap()
        .nlink();
    assert_eq!(nlink, 4, "link count should be 4");
}

/// EDG-SANDBOX.D regression: `create_hardlinks` now returns
/// `io::Result<()>` and must surface a non-EACCES `linkat` failure as
/// `Err` instead of debug-logging and continuing with the void pre-fix
/// signature.
///
/// Before this fix, `create_hardlinks` returned `()` and the
/// `Err(e) => debug_log!(...)` branch dropped every error class. An
/// EMLINK (link-count exhaustion), EXDEV (cross-device link),
/// or EEXIST on a planted obstacle all silently skipped the follower
/// while the receiver exited `rc=0` with the hardlink missing.
///
/// This test plants a directory at the follower's destination path so
/// the underlying `linkat`/`hard_link` syscall returns
/// `AlreadyExists` (EEXIST), a non-EACCES class. The fix must surface
/// it as `Err`. EACCES is covered by the discrimination test in the
/// deletion module - one fail-loud regression per site is enough
/// because all sites share the same upstream-parity rule
/// (`PermissionDenied` is the only non-fatal class).
///
/// upstream: hlink.c:maybe_hard_link -> atomic_create - EACCES is
/// non-fatal (increment io_error and continue); every other class is
/// a security boundary the receiver must surface.
#[cfg(unix)]
#[test]
fn create_hardlinks_surfaces_non_eacces_error() {
    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Leader file exists at `leader.txt`. Plant a directory at
    // `follower` so the obstacle-removal logic (`unlink` with
    // `UnlinkFlags::File`) returns EISDIR (swallowed by the
    // pre-existing `let _ = ...` contract), the
    // `lstat`/`metadata` check sees a directory, and the inode
    // comparison fails so the flow reaches the `linkat`. `linkat`
    // refuses to overwrite an existing directory and surfaces
    // AlreadyExists/EEXIST.
    std::fs::write(dest.join("leader.txt"), "shared content").unwrap();
    std::fs::create_dir(dest.join("follower")).expect("plant directory obstacle");

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 42),
        make_hlink_follower("follower", 14, 42),
    ];

    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;

    #[cfg(unix)]
    let result = ctx.create_hardlinks(dest, None, &mut writer);
    #[cfg(not(unix))]
    let result = ctx.create_hardlinks(dest, &mut writer);

    let err =
        result.expect_err("non-EACCES linkat failure must propagate as Err, not be coerced to ()");
    assert_ne!(
        err.kind(),
        std::io::ErrorKind::PermissionDenied,
        "EACCES is the upstream-parity non-fatal branch; this scenario \
         plants a directory at the follower path to exercise the fail-loud \
         branch via AlreadyExists/EEXIST",
    );
    // The planted obstacle must persist - the receiver must not silently
    // replace it while reporting the hardlink failure as `()`.
    assert!(
        dest.join("follower").is_dir(),
        "the planted directory must persist - the receiver must not silently \
         delete or replace it",
    );
    // The receiver-side invariant: tracker must be restored after the
    // error propagates so subsequent incremental segments can still
    // see the recorded leaders.
    assert!(
        ctx.hardlink_tracker.is_some(),
        "the hardlink tracker must be restored before propagating Err so \
         incremental segments preserve their leader-path state",
    );
}

/// Pins the `--delay-updates` + `--hard-links` phase ordering (Rule 9).
///
/// A `--delay-updates` leader is committed under the `.~tmp~` partial-dir and is
/// renamed to its final path only in phase 2 by `handle_delayed_updates`. A
/// follower may be hard-linked to it only *after* that rename - upstream
/// `receiver.c:694-695` (the phase-2 rename) then `:551-552`
/// (`send_msg_success` -> `finish_hard_link`). `create_hardlinks` links the
/// follower against the leader's FINAL path (`dest_dir.join(rel)`), so if it ran
/// before the delayed rename the follower's `linkat` would target a leader still
/// staged under `.~tmp~`: `ENOENT` (a fatal transfer error) or a stale
/// pre-existing inode.
///
/// `finalize_delayed_updates_and_hardlinks` is the single ordering site the four
/// pipelined drivers share. This test stages a leader under `.~tmp~` (its final
/// path absent, exactly as a delayed commit leaves it) and drives the finalize
/// step: the follower must end up hard-linked to the leader at its final path
/// with the committed content. Reversing the two calls inside the helper makes
/// `create_hardlinks` run first and fail with `ENOENT`, failing this test.
#[cfg(unix)]
#[test]
fn finalize_links_followers_after_delayed_leader_rename() {
    use std::os::unix::fs::MetadataExt;

    let temp_dir = tempfile::TempDir::new().unwrap();
    let dest = temp_dir.path();

    // Leader staged under `.~tmp~`, as a `--delay-updates` commit leaves it: its
    // final path `dest/leader.txt` does NOT exist yet.
    let staging = dest.join(".~tmp~");
    std::fs::create_dir_all(&staging).unwrap();
    let payload = b"shared delayed payload\n";
    std::fs::write(staging.join("leader.txt"), payload).unwrap();

    let entries = vec![
        make_hlink_leader("leader.txt", payload.len() as u64, 42),
        make_hlink_follower("follower.txt", payload.len() as u64, 42),
    ];
    let mut ctx = receiver_with_hardlinks(entries);
    let mut writer = TestDeletionWriter;

    // The delayed-updates list the driver hands to the finalize step: rename the
    // staged leader to its final path.
    let delayed = vec![(staging.join("leader.txt"), dest.join("leader.txt"))];

    ctx.finalize_delayed_updates_and_hardlinks(dest, None, &delayed, &mut writer)
        .expect(
            "finalize must succeed: handle_delayed_updates renames the staged \
             leader to its final path before the follower is linked to it",
        );

    let leader = dest.join("leader.txt");
    let follower = dest.join("follower.txt");
    assert!(
        leader.exists(),
        "the delayed leader must be renamed to its final path",
    );
    assert!(
        follower.exists(),
        "the follower must be hard-linked to the final leader path",
    );
    assert_eq!(
        std::fs::read(&leader).unwrap(),
        payload,
        "leader must hold the committed content",
    );
    assert_eq!(
        std::fs::read(&follower).unwrap(),
        payload,
        "follower must carry the leader's content via the shared inode, not a \
         stale or empty file",
    );

    let leader_meta = std::fs::metadata(&leader).unwrap();
    let follower_meta = std::fs::metadata(&follower).unwrap();
    assert_eq!(
        leader_meta.ino(),
        follower_meta.ino(),
        "follower must share the leader's inode at its FINAL path; a follower \
         linked before the delayed rename would target the still-staged leader \
         (ENOENT) or a stale inode",
    );
    assert!(
        leader_meta.nlink() >= 2,
        "leader nlink must be >= 2 for a hard-link pair, got {}",
        leader_meta.nlink(),
    );
    assert!(
        !staging.exists(),
        "the .~tmp~ staging dir must be removed after the delayed rename",
    );
}

/// Builds a client-mode (pull) receiver over the same hardlink fixture so the
/// scope guard in [`ReceiverContext::emit_server_hardlink_follower_itemize`] can
/// be exercised. A pull renders follower rows locally in `create_hardlinks`, so
/// nothing must cross the wire here.
fn pull_receiver_with_hardlinks(entries: Vec<FileEntry>) -> ReceiverContext {
    let handshake = test_handshake();
    let config = ServerConfig {
        role: ServerRole::Receiver,
        protocol: ProtocolVersion::try_from(32u8).unwrap(),
        flag_string: "-logDtpHre.".to_owned(),
        flags: ParsedServerFlags {
            hard_links: true,
            ..Default::default()
        },
        connection: crate::config::ConnectionConfig {
            client_mode: true,
            ..Default::default()
        },
        args: vec![OsString::from(".")],
        ..Default::default()
    };
    let mut ctx = ReceiverContext::new_for_test(&handshake, config);
    ctx.file_list = entries;
    ctx
}

/// A hardlink follower is never sent for transfer, so a server-mode push
/// receiver must forward its itemize record (`NDX + iflags + xname`)
/// explicitly; otherwise the pushing client's sender has nothing to render and
/// the follower's `hf...` / `=> leader` row is silently dropped (issue #119).
/// This pins the exact wire shape upstream emits (`generator.c:585-591`,
/// `hlink.c:218-234`) and proves the peer's own sender decoder round-trips it.
#[test]
fn server_push_emits_follower_ndx_iflags_and_leader_xname() {
    use crate::generator::ItemFlags;
    use protocol::codec::{NdxCodec, create_ndx_codec};

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 42),
        make_hlink_follower("follower.txt", 14, 42),
    ];
    let ctx = receiver_with_hardlinks(entries);
    let expected_ndx = ctx.flat_to_wire_ndx(1);

    let mut buf: Vec<u8> = Vec::new();
    let mut ndx_codec = create_ndx_codec(32);
    ctx.emit_server_hardlink_follower_itemize(&mut buf, &mut ndx_codec)
        .expect("server-mode follower itemize must serialize");
    assert!(
        !buf.is_empty(),
        "a server-mode push must forward the follower itemize record",
    );

    // Decode with the peer sender's own reader path so the test fails if the
    // emitted bytes ever diverge from what the sender consumes.
    let mut cur = std::io::Cursor::new(buf);
    let mut rd = create_ndx_codec(32);
    let ndx = rd.read_ndx(&mut cur).expect("follower NDX must decode");
    assert_eq!(
        ndx, expected_ndx,
        "the follower must be itemized under its own wire NDX",
    );

    let iflags = ItemFlags::read(&mut cur, 32).expect("follower iflags must decode");
    assert!(
        iflags.raw() & ItemFlags::ITEM_XNAME_FOLLOWS != 0,
        "the follower carries ITEM_XNAME_FOLLOWS so the peer expects a leader name",
    );
    assert!(
        iflags.raw() & ItemFlags::ITEM_LOCAL_CHANGE != 0,
        "the follower is a local hardlink change (renders the `h` itemize char)",
    );

    let (fnamecmp_type, xname, _trailing) = iflags
        .read_trailing(&mut cur)
        .expect("the xname vstring must decode");
    assert!(fnamecmp_type.is_none(), "a follower carries no basis type");
    assert_eq!(
        xname.as_deref(),
        Some(b"leader.txt".as_ref()),
        "the xname must name the leader so the peer renders `=> leader.txt`",
    );
    assert_eq!(
        cur.position() as usize,
        cur.get_ref().len(),
        "exactly one follower record, fully consumed, must be on the wire",
    );
}

/// Two followers sharing one leader must each get their own record, in flist
/// order, so the peer renders a row per follower rather than a single collapsed
/// line.
#[test]
fn server_push_emits_one_record_per_follower() {
    use crate::generator::ItemFlags;
    use protocol::codec::{NdxCodec, create_ndx_codec};

    let entries = vec![
        make_hlink_leader("a.txt", 11, 100),
        make_hlink_follower("b.txt", 11, 100),
        make_hlink_follower("c.txt", 11, 100),
    ];
    let ctx = receiver_with_hardlinks(entries);
    let expected = [ctx.flat_to_wire_ndx(1), ctx.flat_to_wire_ndx(2)];

    let mut buf: Vec<u8> = Vec::new();
    let mut ndx_codec = create_ndx_codec(32);
    ctx.emit_server_hardlink_follower_itemize(&mut buf, &mut ndx_codec)
        .expect("server-mode follower itemize must serialize");

    let mut cur = std::io::Cursor::new(buf);
    let mut rd = create_ndx_codec(32);
    for want_ndx in expected {
        let ndx = rd.read_ndx(&mut cur).expect("follower NDX must decode");
        assert_eq!(ndx, want_ndx, "followers must be itemized in flist order");
        let iflags = ItemFlags::read(&mut cur, 32).expect("iflags must decode");
        let (_ft, xname, _n) = iflags.read_trailing(&mut cur).expect("xname must decode");
        assert_eq!(
            xname.as_deref(),
            Some(b"a.txt".as_ref()),
            "every follower names the same shared leader",
        );
    }
    assert_eq!(
        cur.position() as usize,
        cur.get_ref().len(),
        "both follower records must be fully consumed",
    );
}

/// A client-mode (pull) receiver renders follower rows locally via
/// `create_hardlinks`; forwarding them over the wire too would double every
/// follower against the pull's own row. The server-only emission must stay a
/// no-op off a push.
#[test]
fn pull_receiver_emits_no_follower_wire_record() {
    use protocol::codec::create_ndx_codec;

    let entries = vec![
        make_hlink_leader("leader.txt", 14, 42),
        make_hlink_follower("follower.txt", 14, 42),
    ];
    let ctx = pull_receiver_with_hardlinks(entries);

    let mut buf: Vec<u8> = Vec::new();
    let mut ndx_codec = create_ndx_codec(32);
    ctx.emit_server_hardlink_follower_itemize(&mut buf, &mut ndx_codec)
        .expect("client-mode call must succeed as a no-op");
    assert!(
        buf.is_empty(),
        "a pull renders follower rows locally; nothing crosses the wire",
    );
}

/// With no hardlink followers in the file list, a server push must emit nothing
/// - the emission is scoped strictly to the follower path.
#[test]
fn server_push_without_followers_emits_nothing() {
    use protocol::codec::create_ndx_codec;

    let entries = vec![make_hlink_leader("solo.txt", 9, 7)];
    let ctx = receiver_with_hardlinks(entries);

    let mut buf: Vec<u8> = Vec::new();
    let mut ndx_codec = create_ndx_codec(32);
    ctx.emit_server_hardlink_follower_itemize(&mut buf, &mut ndx_codec)
        .expect("no-follower call must succeed");
    assert!(
        buf.is_empty(),
        "a lone leader has no follower rows to forward",
    );
}
