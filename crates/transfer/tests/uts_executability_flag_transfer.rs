//! Port of the upstream rsync 3.4.4 testsuite `executability.test`.
//!
//! Upstream source of truth:
//!   `target/interop/upstream-src/rsync-3.4.4/testsuite/executability.test`
//!   `rsync.c` `dest_mode()` - when `-E` is set without `-p`, only the
//!   executability bits transfer from source to destination.
//!
//! Why this matters: `--executability` (`-E`) is a narrow, easy-to-break
//! permission-transfer mode. Without `-p` (preserve-perms), a plain recursive
//! copy must NOT touch the destination's mode at all - the receiver keeps
//! whatever the file already had. Only when `-E` is added should the *exec bits
//! alone* follow the source, leaving the read/write bits on the destination
//! untouched. A regression here silently corrupts permissions: either a copy
//! rewrites modes it should have left alone, or `-E` fails to propagate the
//! exec bit and a script arrives non-executable.
//!
//! The existing unit test (`metadata::executability_entry_path`) exercises
//! `apply_metadata_from_file_entry` in isolation. This test guards the
//! end-to-end CLI transfer, including the upstream test's key negative leg:
//! "No -E, so nothing should have changed."

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use filetime::{FileTime, set_file_mtime};
use test_support::{LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binary};

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("stat file").permissions().mode() & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
}

/// Writes `content` and pins mode + a fixed mtime so a later `-t` transfer
/// takes the quick-check (attribute-only) path rather than re-sending data.
fn write_fixed(path: &Path, content: &[u8], mode: u32) {
    fs::write(path, content).expect("write file");
    set_mode(path, mode);
    set_file_mtime(path, FileTime::from_unix_time(1_600_000_000, 0)).expect("set mtime");
}

/// Builds the up-to-date source/destination pair used by the remote-shell
/// attribute-only tests: identical bytes and mtimes, differing exec bits.
fn attr_only_fixture(from: &Path, to: &Path) {
    fs::create_dir_all(from).expect("create from dir");
    fs::create_dir_all(to).expect("create to dir");
    // gains exec: source executable, destination not
    write_fixed(&from.join("gain"), b"aaa\n", 0o755);
    write_fixed(&to.join("gain"), b"aaa\n", 0o644);
    // loses exec: source not executable, destination is
    write_fixed(&from.join("lose"), b"bbb\n", 0o644);
    write_fixed(&to.join("lose"), b"bbb\n", 0o755);
    // keeps exec: both sides carry an exec bit, dest bits stay verbatim
    write_fixed(&from.join("keep"), b"ccc\n", 0o755);
    write_fixed(&to.join("keep"), b"ccc\n", 0o654);
}

/// Asserts the upstream `dest_mode()` outcome for [`attr_only_fixture`].
///
/// upstream: rsync.c:449-473 dest_mode() - existing dest keeps its perm bits;
/// with `-E` a non-executable source strips 0111, an executable source grants
/// exec to every class that can read, and a dest that already has any exec
/// bit is left verbatim.
fn assert_attr_only_outcome(to: &Path) {
    assert_eq!(
        mode_of(&to.join("gain")),
        0o755,
        "-E must grant exec to every readable class on an up-to-date dest"
    );
    assert_eq!(
        mode_of(&to.join("lose")),
        0o644,
        "-E must strip exec bits when the source is not executable"
    );
    assert_eq!(
        mode_of(&to.join("keep")),
        0o654,
        "-E must leave a dest that already has an exec bit verbatim"
    );
}

/// Full three-phase replay of the upstream `executability.test`: a plain
/// recursive copy leaves destination modes alone, and only `-E` transfers the
/// executability bits without disturbing the read/write bits.
#[test]
fn executability_flag_transfers_only_exec_bits() {
    if !require_binary("oc-rsync") {
        return;
    }

    let tmp = create_tempdir();
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    fs::create_dir_all(&from).expect("create from dir");
    let src = format!("{}/", from.display());
    let dst = format!("{}/", to.display());

    let f1 = from.join("1");
    let f2 = from.join("2");
    fs::write(&f1, b"#!/bin/sh\necho 'Program One!'\n").expect("write 1");
    fs::write(&f2, b"#!/bin/sh\necho 'Program Two!'\n").expect("write 2");
    // Upstream uses 1700 (sticky + rwx) on file 1; the sticky bit is
    // irrelevant to executability so we use plain 0700 for a portable check.
    set_mode(&f1, 0o700);
    set_mode(&f2, 0o600);

    // Phase 1: first copy with no -E and no -p. New files are created, so the
    // receiver applies the source mode's exec bits on creation (upstream ends
    // with 1's owner-x present, 2 without). We assert exec-bit parity with the
    // source rather than exact bytes to stay portable across umask.
    OcRsyncCliRunner::new()
        .arg("-r")
        .arg(&src)
        .arg(&dst)
        .run()
        .expect("run 1")
        .assert_success();
    let d1 = to.join("1");
    let d2 = to.join("2");
    assert_eq!(
        mode_of(&d1) & 0o100,
        0o100,
        "file 1 was executable at source; a fresh copy must arrive executable"
    );
    assert_eq!(
        mode_of(&d2) & 0o111,
        0,
        "file 2 was non-executable at source; a fresh copy must arrive non-executable"
    );

    // Phase 2: perturb both trees, then re-copy WITHOUT -E. This is upstream's
    // "No -E, so nothing should have changed" leg: the receiver must leave the
    // existing destination modes exactly as they are.
    set_mode(&f1, 0o600);
    set_mode(&f2, 0o601);
    set_mode(&d1, 0o700);
    set_mode(&d2, 0o604);
    OcRsyncCliRunner::new()
        .arg("-r")
        .arg(&src)
        .arg(&dst)
        .run()
        .expect("run 2 (no -E)")
        .assert_success();
    assert_eq!(
        mode_of(&d1),
        0o700,
        "without -E or -p, an existing destination's mode must be left untouched (file 1)"
    );
    assert_eq!(
        mode_of(&d2),
        0o604,
        "without -E or -p, an existing destination's mode must be left untouched (file 2)"
    );

    // Phase 3: re-copy WITH -E. Now the exec bits follow the source while the
    // read/write bits on the destination stay put. Source 1 is now 0600
    // (no exec) so dest 1 must lose its exec bit; source 2 is 0601 (owner-x)
    // so dest 2 must gain exec for every class that can already read.
    OcRsyncCliRunner::new()
        .arg("-r")
        .arg("-E")
        .arg(&src)
        .arg(&dst)
        .run()
        .expect("run 3 (-E)")
        .assert_success();
    assert_eq!(
        mode_of(&d1) & 0o111,
        0,
        "-E with a now-non-executable source must clear dest 1's exec bits"
    );
    assert_eq!(
        mode_of(&d1) & 0o666,
        0o600,
        "-E must leave dest 1's read/write bits untouched"
    );
    assert_eq!(
        mode_of(&d2) & 0o100,
        0o100,
        "-E with an executable source must grant exec on dest 2"
    );
    assert_eq!(
        mode_of(&d2) & 0o666,
        0o604,
        "-E must leave dest 2's read/write bits untouched"
    );
}

/// Regression: `-rtE` over a remote shell (push) must chmod up-to-date files.
///
/// With identical bytes and mtimes the quick check skips the transfer, so the
/// only way the exec bits can follow the source is the receiver's
/// attribute-only pass. The network receiver used to skip that pass because
/// `metadata_unchanged` never consulted `--executability`, leaving the
/// destination at its old mode while the itemized output claimed a `p` change.
///
/// upstream: generator.c:418-426 perms_differ() feeds unchanged_attrs(), so an
/// executability-presence mismatch forces set_file_attrs() on the skip path
/// (generator.c:1827).
#[test]
fn executability_applies_on_up_to_date_files_over_rsh_push() {
    if !require_binary("oc-rsync") || !require_binary(LSH_STUB_BIN) {
        return;
    }
    let stub = LshRunnerStub::locate().expect("lsh-stub located");

    let tmp = create_tempdir();
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    attr_only_fixture(&from, &to);

    OcRsyncCliRunner::new()
        .arg("-rtE")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("{}/", from.display()))
        .arg(format!("localhost:{}/", to.display()))
        .run()
        .expect("push run")
        .assert_success();

    assert_attr_only_outcome(&to);
}

/// Regression: `-rtE` over a remote shell (pull) must chmod up-to-date files.
///
/// Same attribute-only scenario as the push test, but the local client is the
/// receiver. Note `-E` never rides the wire on a pull (options.c:2692 packs
/// 'E' only when `am_sender`); the local receiver must honour its own flag.
#[test]
fn executability_applies_on_up_to_date_files_over_rsh_pull() {
    if !require_binary("oc-rsync") || !require_binary(LSH_STUB_BIN) {
        return;
    }
    let stub = LshRunnerStub::locate().expect("lsh-stub located");

    let tmp = create_tempdir();
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    attr_only_fixture(&from, &to);

    OcRsyncCliRunner::new()
        .arg("-rtE")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", from.display()))
        .arg(format!("{}/", to.display()))
        .run()
        .expect("pull run")
        .assert_success();

    assert_attr_only_outcome(&to);
}

/// `-rtpE`: `--perms` wins and `-E` is a no-op - up-to-date files get the
/// source mode copied exactly, including the read/write bits `-E` alone would
/// leave untouched.
///
/// upstream: options.c:2690-2693 - the server arg string carries 'p', never
/// 'E', when both are set; generator.c:418-421 perms_differ() compares the
/// full mode under preserve_perms.
#[test]
fn executability_is_noop_when_perms_active_over_rsh() {
    if !require_binary("oc-rsync") || !require_binary(LSH_STUB_BIN) {
        return;
    }
    let stub = LshRunnerStub::locate().expect("lsh-stub located");

    let tmp = create_tempdir();
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    attr_only_fixture(&from, &to);

    OcRsyncCliRunner::new()
        .arg("-rtpE")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("{}/", from.display()))
        .arg(format!("localhost:{}/", to.display()))
        .run()
        .expect("push run with -p")
        .assert_success();

    assert_eq!(mode_of(&to.join("gain")), 0o755, "-p copies the exact mode");
    assert_eq!(mode_of(&to.join("lose")), 0o644, "-p copies the exact mode");
    assert_eq!(
        mode_of(&to.join("keep")),
        0o755,
        "-p overrides -E and copies the exact source mode"
    );
}
