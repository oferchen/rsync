//! Integration tests for the `IORING_OP_RENAMEAT` (RENAMEAT2) wrapper.
//!
//! These tests exercise the real kernel path via [`fast_io::renameat2_blocking`],
//! which builds and submits a single SQE on a transient ring and reaps the
//! resulting CQE. The whole suite is a no-op on non-Linux platforms or when
//! the `io_uring` cargo feature is disabled.
//!
//! Each test short-circuits to a successful skip when
//! [`fast_io::renameat2_supported`] reports `false`, so kernels older than
//! 5.11 (or environments where seccomp blocks io_uring) do not produce
//! spurious failures.

#![cfg(all(target_os = "linux", feature = "io_uring"))]

use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

use fast_io::{
    RENAME_EXCHANGE, RENAME_NOREPLACE, RenameAt2Args, build_renameat2_sqe, is_io_uring_available,
    renameat2_blocking, renameat2_supported,
};

#[test]
fn renameat2_renames_a_real_file() {
    if !renameat2_supported() {
        eprintln!("skipping: IORING_OP_RENAMEAT not supported on this kernel");
        return;
    }
    assert!(is_io_uring_available());

    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("renameat2-src");
    let dst = dir.path().join("renameat2-dst");
    fs::write(&src, b"renameat2 payload").expect("seed source file");

    let src_c = CString::new(src.as_os_str().as_bytes()).expect("nul-free path");
    let dst_c = CString::new(dst.as_os_str().as_bytes()).expect("nul-free path");

    let args = RenameAt2Args {
        old_dir_fd: libc::AT_FDCWD,
        old_path: &src_c,
        new_dir_fd: libc::AT_FDCWD,
        new_path: &dst_c,
        flags: 0,
    };
    let cqe_result = renameat2_blocking(args).expect("blocking submit must succeed");
    assert_eq!(cqe_result, 0, "RENAMEAT CQE returned errno {cqe_result}");

    assert!(!src.exists(), "source must be unlinked after rename");
    assert!(dst.exists(), "destination must exist after rename");
    let content = fs::read(&dst).expect("destination readable");
    assert_eq!(content, b"renameat2 payload");
}

#[test]
fn renameat2_noreplace_rejects_existing_destination() {
    if !renameat2_supported() {
        eprintln!("skipping: IORING_OP_RENAMEAT not supported on this kernel");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let src = dir.path().join("noreplace-src");
    let dst = dir.path().join("noreplace-dst");
    fs::write(&src, b"src").expect("seed source");
    {
        let mut f = fs::File::create(&dst).expect("seed destination");
        f.write_all(b"dst").expect("write");
    }

    let src_c = CString::new(src.as_os_str().as_bytes()).expect("nul-free");
    let dst_c = CString::new(dst.as_os_str().as_bytes()).expect("nul-free");

    let args = RenameAt2Args {
        old_dir_fd: libc::AT_FDCWD,
        old_path: &src_c,
        new_dir_fd: libc::AT_FDCWD,
        new_path: &dst_c,
        flags: RENAME_NOREPLACE,
    };
    let cqe_result = renameat2_blocking(args).expect("blocking submit must succeed");
    assert_eq!(
        cqe_result,
        -libc::EEXIST,
        "RENAME_NOREPLACE must fail with -EEXIST when destination exists"
    );
    assert_eq!(fs::read(&src).unwrap(), b"src");
    assert_eq!(fs::read(&dst).unwrap(), b"dst");
}

#[test]
fn renameat2_exchange_swaps_two_files() {
    if !renameat2_supported() {
        eprintln!("skipping: IORING_OP_RENAMEAT not supported on this kernel");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let a = dir.path().join("exchange-a");
    let b = dir.path().join("exchange-b");
    fs::write(&a, b"AAAA").expect("seed a");
    fs::write(&b, b"BBBB").expect("seed b");

    let a_c = CString::new(a.as_os_str().as_bytes()).expect("nul-free");
    let b_c = CString::new(b.as_os_str().as_bytes()).expect("nul-free");

    let args = RenameAt2Args {
        old_dir_fd: libc::AT_FDCWD,
        old_path: &a_c,
        new_dir_fd: libc::AT_FDCWD,
        new_path: &b_c,
        flags: RENAME_EXCHANGE,
    };
    let cqe_result = renameat2_blocking(args).expect("blocking submit must succeed");
    if cqe_result == -libc::EINVAL {
        // Some filesystems (older overlayfs, certain tmpfs configurations)
        // report EXCHANGE as unsupported even when the kernel opcode itself
        // is available. Treat this as a skip rather than a failure.
        eprintln!("skipping exchange test: filesystem does not support RENAME_EXCHANGE");
        return;
    }
    assert_eq!(
        cqe_result, 0,
        "RENAME_EXCHANGE CQE returned errno {cqe_result}"
    );

    assert_eq!(fs::read(&a).unwrap(), b"BBBB", "a must now hold b's data");
    assert_eq!(fs::read(&b).unwrap(), b"AAAA", "b must now hold a's data");
}

#[test]
fn renameat2_supported_is_idempotent() {
    let first = renameat2_supported();
    let second = renameat2_supported();
    assert_eq!(first, second);
}

#[test]
fn build_renameat2_sqe_agrees_with_probe() {
    let old = CString::new("/tmp/probe-agreement-old").unwrap();
    let new = CString::new("/tmp/probe-agreement-new").unwrap();
    let args = RenameAt2Args {
        old_dir_fd: libc::AT_FDCWD,
        old_path: &old,
        new_dir_fd: libc::AT_FDCWD,
        new_path: &new,
        flags: 0,
    };
    let result = build_renameat2_sqe(args);
    if renameat2_supported() {
        assert!(result.is_ok());
    } else {
        let err = result.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }
}
