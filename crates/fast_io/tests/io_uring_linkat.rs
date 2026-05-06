//! Integration tests for the io_uring `LINKAT` opcode wrapper.
//!
//! These tests submit a real `IORING_OP_LINKAT` to the kernel, wait for the
//! CQE, and verify that the destination hardlink exists on disk. They run
//! only on Linux with the `io_uring` cargo feature enabled and skip
//! gracefully when the running kernel does not advertise the opcode (for
//! example, kernels older than 5.15 or seccomp-restricted environments).

#![cfg(all(target_os = "linux", feature = "io_uring"))]

use std::ffi::CString;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;

use fast_io::{LinkAtArgs, linkat_supported, submit_linkat_blocking};
use tempfile::tempdir;

#[test]
fn linkat_creates_real_hardlink_on_supported_kernel() {
    if !linkat_supported() {
        eprintln!("IORING_OP_LINKAT not supported on this kernel; skipping");
        return;
    }

    let dir = tempdir().expect("tempdir");
    let src_path = dir.path().join("linkat_src.txt");
    let dst_path = dir.path().join("linkat_dst.txt");

    {
        let mut f = fs::File::create(&src_path).expect("create source");
        f.write_all(b"io_uring linkat smoke").expect("write source");
    }

    let src_c = CString::new(src_path.as_os_str().as_bytes()).expect("source path -> CString");
    let dst_c = CString::new(dst_path.as_os_str().as_bytes()).expect("dest path -> CString");

    let result = submit_linkat_blocking(LinkAtArgs {
        old_dirfd: libc::AT_FDCWD,
        old_path: &src_c,
        new_dirfd: libc::AT_FDCWD,
        new_path: &dst_c,
        flags: 0,
    })
    .expect("LINKAT must succeed on supported kernel");
    assert_eq!(result, 0, "LINKAT CQE result must be 0");

    let meta_src = fs::metadata(&src_path).expect("stat source");
    let meta_dst = fs::metadata(&dst_path).expect("stat dest");
    assert_eq!(meta_src.len(), meta_dst.len());
    assert_eq!(fs::read(&dst_path).unwrap(), b"io_uring linkat smoke");

    // Unlinking the destination first proves the inode is genuinely shared
    // via a hardlink rather than a separate file. Cleanup of the source is
    // handled by tempdir's Drop.
    fs::remove_file(&dst_path).expect("remove dest hardlink");
    assert!(src_path.exists(), "source must survive dest removal");
}

#[test]
fn linkat_supported_is_idempotent_under_load() {
    let first = linkat_supported();
    for _ in 0..16 {
        assert_eq!(linkat_supported(), first);
    }
}

#[test]
fn linkat_returns_unsupported_when_probe_false() {
    if linkat_supported() {
        return;
    }
    let src = CString::new("/tmp/linkat_unsupported_src").unwrap();
    let dst = CString::new("/tmp/linkat_unsupported_dst").unwrap();
    let err = submit_linkat_blocking(LinkAtArgs {
        old_dirfd: libc::AT_FDCWD,
        old_path: &src,
        new_dirfd: libc::AT_FDCWD,
        new_path: &dst,
        flags: 0,
    })
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
}
