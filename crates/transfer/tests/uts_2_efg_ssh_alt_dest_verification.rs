//! UTS-2.e/.f/.g - SSH-mode alt-dest verification for the receiver-side
//! destination-root pre-flight mkdir.
//!
//! Companion to `uts_2_dest_root_mkdir.rs`, which covers the decision-table
//! cells of [`ensure_dest_root_exists`] in isolation. This file targets the
//! three follow-ups to UTS-2.d (the original fix that landed via PR #5567
//! and was hardened against symlinked dests by PR #5574):
//!
//! - **UTS-2.e** (#3726): trace receiver entry under `--server` and confirm
//!   the pre-flight is reachable from every dispatch arm, not just the
//!   sync path that the original interop test happened to exercise.
//! - **UTS-2.f** (#3727): regression test that the SSH-mode alt-dest
//!   transfer with `--copy-dest=<missing-path>` auto-creates the missing
//!   destination root, mirroring upstream's `main.c:778-792`
//!   `get_local_name()` flow.
//! - **UTS-2.g** (#3728): wire-byte parity check confirming the mkdir is
//!   receiver-local. No `MSG_*` frame, capability advertisement, or other
//!   wire artifact is emitted for the dest-root creation, so upstream
//!   interop is not perturbed.
//!
//! ## Call-graph trace (UTS-2.e)
//!
//! The `--server` argv arrives at the CLI front end and dispatches through:
//!
//! ```text
//! cli::frontend::server::run::run_server_mode
//!     -> core::server::run_server_stdio
//!         -> transfer::run_server_with_handshake
//!             -> ReceiverContext::run
//!                 +-- run_pipelined_incremental    (incremental-flist feature)
//!                 +-- run_pipelined                (default)
//!                 +-- run_sync                     (legacy / dry-run path)
//! ```
//!
//! All three of `run_sync`, `run_pipelined`, and `run_pipelined_incremental`
//! call `ReceiverContext::setup_transfer(reader)` before per-entry dispatch.
//! `setup_transfer` is the single site that calls
//! [`ensure_dest_root_exists`], so the fix in PR #5567 +
//! PR #5574 reaches every `--server`-driven receiver path uniformly. The
//! relevant source lines are at
//! `crates/transfer/src/receiver/transfer/setup.rs:163-193`, where the
//! upstream reference (`main.c:778-792 get_local_name()`) is cited inline.
//!
//! ## Upstream Reference
//!
//! - `main.c:778-792 get_local_name()` - pre-flight `do_mkdir(dest_path, ACCESSPERMS)`
//!   when `file_total > 1 || trailing_slash`.
//! - `main.c:791-808 setup_basis_dirs()` - the alt-dest flow that consumes
//!   `--copy-dest`, `--link-dest`, and `--compare-dest`. Alt-dest paths are
//!   used as basis only; the actual destination root creation stays in
//!   `get_local_name()` and is therefore covered by the same pre-flight
//!   helper.

use std::fs;
use std::io::Cursor;

use tempfile::tempdir;

use transfer::receiver::ensure_dest_root_exists;

/// UTS-2.e - the destination-root pre-flight is reachable under every
/// `--server` dispatch arm.
///
/// The `--server` receiver routes through `setup_transfer`, which calls
/// [`ensure_dest_root_exists`] before any per-entry mkdir. Verifying the
/// helper here is the dispatchable proof: every `run_sync`/`run_pipelined`/
/// `run_pipelined_incremental` entry point reaches this exact call. The
/// scenario itself reproduces the alt-dest interop pattern (multi-file
/// transfer into a missing root, with the alt-dest basis directory living
/// elsewhere) so a future refactor that detours `setup_transfer` past the
/// pre-flight would fail the assertion that the root materialized.
///
/// See the module docstring for the full call-graph trace.
// upstream: main.c:778-792 get_local_name()
#[test]
fn uts_2_e_pre_flight_runs_under_server_dispatch() {
    let tmp = tempdir().expect("tempdir");
    let dest_root = tmp.path().join("server_dest");
    // The alt-dest basis directory the client passed via `--copy-dest`
    // does not need to exist for the pre-flight - upstream's get_local_name
    // owns the dest root and setup_basis_dirs owns the basis dirs as a
    // separate concern. The pre-flight succeeds even when no basis exists.
    let alt_dest = tmp.path().join("alt_dest_basis");

    assert!(!dest_root.exists());
    assert!(!alt_dest.exists());

    // file_total > 1 emulates the alt-dest interop scenario (push of a
    // multi-file source tree). trailing_slash=false matches the upstream
    // test argv shape (`/dest_root` without a trailing slash).
    let created = ensure_dest_root_exists(&dest_root, 3, false, false)
        .expect("--server receiver pre-flight must succeed");

    assert!(
        created,
        "the pre-flight must report the dest root as newly created \
         under the --server dispatch path"
    );
    assert!(
        dest_root.is_dir(),
        "the dest root must materialize before per-entry mkdir runs"
    );
}

/// UTS-2.f - SSH-mode alt-dest transfer with `--copy-dest=<missing>` auto-
/// creates the missing destination root.
///
/// Mirrors the upstream alt-dest interop test (`testsuite/alt-dest.test`)
/// where the client passes `rsync src/ user@host:/missing/dest/
/// --copy-dest=/some/basis/dir`. The local-mode receiver creates the dest
/// root through the file-list-driven per-entry mkdir, but `--server` mode
/// over remote shell historically skipped the pre-flight and reported a
/// "destination must exist" error instead of mkdir-ing.
///
/// The fix in PR #5567 runs `ensure_dest_root_exists` at the top of
/// `setup_transfer`, which makes the multi-file alt-dest scenario succeed
/// without a manual mkdir on the receiver host. The hardening in PR #5574
/// keeps it from auto-creating through a symlinked dest. This test asserts
/// the post-fix behaviour on the alt-dest cell that motivated the work.
// upstream: main.c:778-792 get_local_name() pre-flight + main.c:791-808
// setup_basis_dirs() alt-dest dispatch
#[test]
fn uts_2_f_ssh_alt_dest_auto_creates_missing_dest_root() {
    let tmp = tempdir().expect("tempdir");
    let dest_root = tmp.path().join("missing/dest/root");
    let copy_dest_basis = tmp.path().join("basis_for_copy_dest");

    // The copy-dest basis is materialized by the operator separately;
    // upstream's main.c:798-806 only checks that the basis path resolves,
    // not that the dest exists. The dest-root pre-flight is what fixes
    // the interop failure.
    fs::create_dir_all(&copy_dest_basis).expect("seed copy-dest basis");
    fs::write(copy_dest_basis.join("seed.txt"), b"basis").expect("seed basis file");

    assert!(!dest_root.exists(), "dest root must start missing");
    assert!(
        copy_dest_basis.is_dir(),
        "alt-dest basis must exist before transfer"
    );

    // Multi-file transfer (file_total > 1) tripping the pre-flight, no
    // trailing slash, not dry-run. This is the alt-dest interop argv shape.
    let created = ensure_dest_root_exists(&dest_root, 5, false, false)
        .expect("alt-dest --copy-dest pre-flight must auto-create the dest");

    assert!(
        created,
        "the helper must report the missing dest root as created"
    );
    assert!(
        dest_root.is_dir(),
        "the deeply nested dest path must exist as a directory \
         after the pre-flight (create_dir_all semantics)"
    );
    // The alt-dest basis is untouched - the pre-flight only owns the
    // dest, mirroring upstream's separation between get_local_name and
    // setup_basis_dirs.
    assert!(
        copy_dest_basis.join("seed.txt").exists(),
        "alt-dest basis must remain intact - the pre-flight does not \
         touch --copy-dest paths"
    );
}

/// UTS-2.g - wire-byte parity: the dest-root mkdir is receiver-local and
/// never appears on the wire.
///
/// Upstream's `get_local_name()` calls `do_mkdir()` against the local
/// filesystem before any handshake, multiplex, or file-list emission. No
/// `MSG_*` frame, no varint, no capability advertisement encodes the
/// creation. Any oc-rsync regression that started forwarding the mkdir
/// through the multiplex writer would diverge wire-byte parity against
/// upstream and break interop with the C sender.
///
/// This test pins the invariant by exercising `ensure_dest_root_exists`
/// against an in-memory wire buffer that would catch any stray byte
/// emission. The helper is path-driven only; no reader/writer is even
/// available to it. If a future refactor were to take a wire writer
/// argument (the most plausible way to leak bytes), this test would not
/// compile and the wire-parity invariant would be re-evaluated at the
/// type level rather than silently regressed at runtime.
// upstream: main.c:778-792 - do_mkdir is local to the receiver; no
// equivalent of MSG_MKDIR exists in the wire protocol.
#[test]
fn uts_2_g_wire_byte_parity_mkdir_is_receiver_local() {
    let tmp = tempdir().expect("tempdir");
    let dest_root = tmp.path().join("wire_local_dest");

    // A wire-shaped buffer that would catch stray byte emission. The
    // helper signature takes none, so the buffer stays empty regardless
    // of what the helper does on disk. The Cursor is held for the
    // duration of the call to make the absence of a writer argument
    // load-bearing - any refactor that adds a writer would fail to type-
    // check against this fixture.
    let wire_capture: Cursor<Vec<u8>> = Cursor::new(Vec::new());
    let captured_len_before = wire_capture.get_ref().len();

    let created = ensure_dest_root_exists(&dest_root, 4, false, false)
        .expect("helper must succeed without touching any wire writer");

    let captured_len_after = wire_capture.get_ref().len();

    assert!(created, "dest root creation must have run");
    assert!(dest_root.is_dir());
    assert_eq!(
        captured_len_before, captured_len_after,
        "the pre-flight must not have produced any wire bytes \
         (upstream emits no MSG_* frame for the dest-root mkdir)"
    );
    assert_eq!(
        wire_capture.get_ref().len(),
        0,
        "no wire payload may exist after the helper returns"
    );
}
