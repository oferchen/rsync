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

use test_support::{OcRsyncCliRunner, create_tempdir, require_binary};

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("stat file").permissions().mode() & 0o7777
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("set mode");
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
