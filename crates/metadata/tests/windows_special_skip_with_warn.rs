//! Regression coverage for the WIND-2 skip-with-warn strategy on Windows
//! targets (WIND-3 implementation, WIND-4 test).
//!
//! Upstream rsync's `syscall.c:do_mknod()` materialises FIFOs, sockets, and
//! device nodes via `mknod(2)` / `mkfifo(3)` / `bind(AF_UNIX)`. None of
//! these are available on native (non-Cygwin) Windows, so the receiver
//! cannot reproduce the source-side inode. Per
//! `docs/design/windows-device-file-strategy.md` the documented behaviour
//! is: emit a warning identifying the path and entry kind, leave the
//! destination untouched, and return `Ok(())` so the surrounding transfer
//! continues with the next entry rather than aborting or silently
//! reporting success.
//!
//! Before WIND-3, the `cfg(not(unix))` arms of `create_fifo_inner` and
//! `create_device_node_inner` were silent `Ok(())` no-ops with no
//! diagnostic. This test pins the new contract:
//!
//! 1. The call returns `Ok(())`.
//! 2. No destination inode is created.
//! 3. A second call against the same path remains a no-op (idempotency).
//!
//! Message-text assertions live in `special.rs`'s in-crate test module
//! where the `pub(crate)` formatting helper is reachable; this file
//! covers the public-API behavioural contract.

#![cfg(not(unix))]

use std::fs;

use metadata::{create_device_node, create_fifo};

/// WIND-2 contract for FIFO entries on a non-Unix target.
#[test]
fn create_fifo_skips_without_writing_destination() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let source_path = temp.path().join("source");
    fs::File::create(&source_path).expect("create source placeholder");
    let metadata = fs::metadata(&source_path).expect("read source metadata");

    let fifo_path = temp.path().join("would-be.fifo");
    assert!(
        !fifo_path.exists(),
        "precondition: destination must not exist before the call",
    );

    create_fifo(&fifo_path, &metadata).expect("skip-with-warn returns Ok");

    assert!(
        !fifo_path.exists(),
        "skip-with-warn must not register a destination inode",
    );
}

/// WIND-2 contract for device entries on a non-Unix target.
#[test]
fn create_device_node_skips_without_writing_destination() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let source_path = temp.path().join("source");
    fs::File::create(&source_path).expect("create source placeholder");
    let metadata = fs::metadata(&source_path).expect("read source metadata");

    let device_path = temp.path().join("would-be.dev");
    assert!(
        !device_path.exists(),
        "precondition: destination must not exist before the call",
    );

    create_device_node(&device_path, &metadata).expect("skip-with-warn returns Ok");

    assert!(
        !device_path.exists(),
        "skip-with-warn must not register a destination inode",
    );
}

/// Repeated invocations against the same path must remain a no-op: nothing
/// is created on the first call, nothing on subsequent calls. This guards
/// against a future regression that, for example, caches the path and then
/// writes a placeholder on the second invocation.
#[test]
fn skip_with_warn_is_idempotent_across_repeated_calls() {
    let temp = tempfile::tempdir().expect("create tempdir");
    let source_path = temp.path().join("source");
    fs::File::create(&source_path).expect("create source placeholder");
    let metadata = fs::metadata(&source_path).expect("read source metadata");

    let fifo_path = temp.path().join("repeated.fifo");
    create_fifo(&fifo_path, &metadata).expect("first call returns Ok");
    create_fifo(&fifo_path, &metadata).expect("second call returns Ok");
    assert!(
        !fifo_path.exists(),
        "neither invocation may create a destination inode",
    );

    let device_path = temp.path().join("repeated.dev");
    create_device_node(&device_path, &metadata).expect("first call returns Ok");
    create_device_node(&device_path, &metadata).expect("second call returns Ok");
    assert!(
        !device_path.exists(),
        "neither invocation may create a destination inode",
    );
}
