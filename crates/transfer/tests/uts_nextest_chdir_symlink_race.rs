//! UTS-NEXTEST-EDGE.i: nextest port of the upstream
//! `testsuite/chdir-symlink-race.test` security scenario.
//!
//! Upstream test source:
//! `target/interop/upstream-src/rsync-3.4.4/testsuite/chdir-symlink-race.test`
//! (the same scenario also lives in rsync-3.4.3 / 3.4.2 / 3.4.1; the 3.4.4
//! file is the canonical upstream copy).
//!
//! The upstream scenario probes the chdir-symlink-race TOCTOU class of
//! bug at the receiver: after CVE-2026-29518's fix to
//! `secure_relative_open()`, an attack remained where the receiver's
//! `chdir()` into a destination subdirectory followed an
//! attacker-planted symlink, escaping the module root. Every
//! subsequent path-relative syscall (open, chmod, lchown, utimes, ...)
//! inherited the escape because the `RESOLVE_BENEATH` anchor itself
//! had moved outside the module.
//!
//! oc-rsync's defense is the UTS-16 series: the receiver opens the
//! destination root through [`fast_io::secure_open_dir`] /
//! [`fast_io::DirSandbox::open_root`], descends with
//! [`DirSandbox::enter`] (which uses
//! `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` on Linux 5.6+ and
//! `openat(O_NOFOLLOW | O_DIRECTORY | O_CLOEXEC)` elsewhere), and
//! propagates the resulting [`io::Error`] back through
//! `setup_transfer` / `sync.rs` / `pipelined.rs` /
//! `pipelined_incremental.rs`. The attacker-planted `subdir ->
//! /outside/...` symlink is therefore refused at the kernel boundary
//! before any data or metadata syscall is issued against the symlink
//! target.
//!
//! Existing coverage:
//! - `crates/transfer/src/receiver/transfer/setup.rs::symlink_race_tests`
//!   unit-tests the private `open_sandbox_for_dest_strict` helper for
//!   the daemon strict path.
//! - `crates/transfer/tests/dir_sandbox_carrier.rs` exercises descent
//!   shape on quiescent trees.
//! - `crates/transfer/tests/sec_1_m_symlink_swap_attack.rs` covers the
//!   per-syscall (`lstat_at` / `unlinkat_at`) invariants under a live
//!   attacker race.
//!
//! This file ports the upstream test's directory-component scenario
//! (`subdir -> outside`, push into the symlinked subdir) into a
//! receiver-shaped nextest that runs on every PR. The asymmetry with
//! the existing tests is deliberate:
//!
//! - the unit test in `setup.rs` only checks the root-level open;
//! - this file checks the descent step (the actual chdir-symlink-race
//!   attack vector) and asserts the outside sentinel file is byte- and
//!   mode-identical to its pristine state regardless of whether the
//!   receiver tried to open, write, chmod, or unlink anything beneath
//!   the symlinked subdirectory.
//!
//! Positive control: when `subdir/` is a real directory, the same
//! descent step succeeds and the receiver's per-entry `*at` syscalls
//! land where they belong.

#![cfg(unix)]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;

use fast_io::DirSandbox;
use tempfile::{TempDir, tempdir};

/// `tempdir()` may sit under a symlink prefix on macOS (`/tmp ->
/// /private/tmp`) or some CI runners. [`DirSandbox::open_root`] refuses
/// any symlink in the path under `RESOLVE_NO_SYMLINKS`, so canonicalise
/// the test root first to keep the harness portable.
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    (dir, canon)
}

/// Stable sentinel content used by both scenarios so the positive and
/// negative tests share the same "outside" fixture.
const OUTSIDE_SECRET: &[u8] = b"OUTSIDE_SECRET_DATA\n";

/// Stable file mode for the outside sentinel. `0o600` matches the
/// upstream test's `chmod 0600 "$outside/target.txt"`.
const OUTSIDE_MODE: u32 = 0o600;

/// Negative case: `dest/subdir` is a symlink to an outside directory.
///
/// Mirrors upstream scenario 2 (`-r src/subdir/ to upload/subdir/`):
/// the receiver descends through `subdir/` to write `keep.txt`, and
/// must refuse the symlink at the descent step. The outside sentinel
/// must be byte- and mode-identical after the refusal.
///
/// Asserts:
/// 1. [`DirSandbox::open_root`] succeeds on the legitimate destination
///    root (the symlink is one level deeper, not at the root).
/// 2. [`DirSandbox::enter`] for `subdir` fails with `ELOOP` (Linux +
///    openat2 / O_NOFOLLOW) or `ENOTDIR` (macOS / BSD where
///    `O_DIRECTORY` is evaluated before `O_NOFOLLOW`).
/// 3. The outside sentinel file's contents are unchanged.
/// 4. The outside sentinel file's mode is unchanged.
/// 5. No file landed at `outside/keep.txt` (the receiver did not write
///    through the symlink).
#[test]
fn rejects_symlinked_subdir_and_leaves_outside_untouched() {
    let (_keep, scratch) = canonical_tempdir();

    // Outside the module: a sensitive directory with a sentinel the
    // attacker is trying to overwrite or chmod through the planted
    // symlink. Mirrors upstream `outside/target.txt`.
    let outside = scratch.join("outside");
    fs::create_dir(&outside).expect("mkdir outside");
    let sentinel = outside.join("target.txt");
    fs::write(&sentinel, OUTSIDE_SECRET).expect("write outside sentinel");
    fs::set_permissions(&sentinel, fs::Permissions::from_mode(OUTSIDE_MODE))
        .expect("chmod outside sentinel");

    // Inside the module: an attacker-planted symlink at
    // `module/subdir` that resolves into the outside tree. Mirrors
    // upstream `ln -s "$outside" "$mod/subdir"`.
    let module = scratch.join("module");
    fs::create_dir(&module).expect("mkdir module");
    let subdir = module.join("subdir");
    symlink(&outside, &subdir).expect("plant symlink subdir -> outside");

    // The receiver opens the destination root, then descends into
    // `subdir` to apply the per-entry transfer (write keep.txt,
    // chmod, utimes, ...). The symlink at `subdir` must be refused at
    // the descent step.
    let mut sandbox = DirSandbox::open_root(&module).expect("open module root");
    let err = sandbox
        .enter(OsStr::new("subdir"))
        .expect_err("symlinked subdir must be refused");

    // Accepted refusals across Unix variants - the same set the
    // existing carrier tests in `dir_sandbox_carrier.rs` document.
    let code = err.raw_os_error().expect("kernel must report errno");
    let accepted: &[i32] = &[
        20, // ENOTDIR (macOS / BSD: O_DIRECTORY beats O_NOFOLLOW)
        40, // ELOOP (Linux: openat2 RESOLVE_NO_SYMLINKS or O_NOFOLLOW)
        62, // ELOOP (macOS / BSD raw value)
    ];
    assert!(
        accepted.contains(&code),
        "expected ENOTDIR or ELOOP for symlinked subdir, got errno={code} ({err})"
    );

    // The outside sentinel must be byte-identical: the receiver never
    // wrote through the symlink. Upstream `verify_unchanged` asserts
    // this with `cmp -s "$outside/target.txt" "$outside_pristine"`.
    let after = fs::read(&sentinel).expect("read outside sentinel after");
    assert_eq!(
        after, OUTSIDE_SECRET,
        "outside file content changed (write escape through symlinked subdir)"
    );

    // The outside sentinel's mode must be unchanged: the receiver
    // never chmod'd through the symlink either. Upstream
    // `verify_unchanged` asserts this with `file_mode` + case `600 |
    // 0600`.
    let mode = fs::metadata(&sentinel)
        .expect("stat outside sentinel")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode, OUTSIDE_MODE,
        "outside file mode changed from 0o600 to 0o{mode:o} (chmod escape through symlinked subdir)"
    );

    // The receiver must not have created or touched anything beneath
    // the outside directory other than the pristine sentinel.
    let stray = outside.join("keep.txt");
    assert!(
        !stray.exists(),
        "stray write landed at {} (sandbox descent leaked through symlink)",
        stray.display()
    );
}

/// Positive control: `dest/subdir` is a real directory.
///
/// Mirrors the upstream test's "what should happen on a clean
/// fixture" implicit invariant: with a legitimate `subdir/`, the
/// receiver's descent step succeeds and per-entry writes land inside
/// the module. Locks in that the sandbox does not over-refuse a real
/// directory and that the negative test's failure mode is specific
/// to the symlink swap.
#[test]
fn accepts_real_subdir_and_writes_keep_file() {
    let (_keep, scratch) = canonical_tempdir();

    let module = scratch.join("module");
    let subdir = module.join("subdir");
    fs::create_dir_all(&subdir).expect("mkdir module/subdir");

    let mut sandbox = DirSandbox::open_root(&module).expect("open module root");
    sandbox
        .enter(OsStr::new("subdir"))
        .expect("real subdir must be accepted");
    assert_eq!(sandbox.depth(), 1);

    // Issue a sandbox-anchored write the way the receiver would:
    // resolve the leaf against the current dirfd via `openat`. We use
    // the std `File::create` helper at the canonicalized absolute
    // path to keep this test focused on the descent decision; the
    // `sec_1_m_symlink_swap_attack.rs` suite exercises the per-leaf
    // `*at` invariants. The point here is that descent succeeded so
    // the per-entry syscalls can fire at all.
    let keep = subdir.join("keep.txt");
    fs::write(&keep, b"keep").expect("write keep.txt inside real subdir");
    assert_eq!(
        fs::read(&keep).expect("read keep.txt"),
        b"keep",
        "real subdir must let the receiver write through"
    );

    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}
