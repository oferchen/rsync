//! The fast_io symlink-owner seam reaches a consumer crate's tests.
//!
//! core enables `fast_io/test-seams` from `[dev-dependencies]` only. This
//! cell proves that is enough: an override armed here changes what fast_io's
//! owner walk decides for core, which is what the single-file destination
//! cells (#8858) rely on to run without root.
#![cfg(unix)]

use std::io::Read;

fn foreign_uid() -> u32 {
    let euid = rustix::process::geteuid().as_raw();
    (1..=8u32)
        .find(|candidate| *candidate != euid)
        .expect("at least one of uids 1..=8 differs from the euid")
}

/// The operator's own symlink is followed; the same link judged as another
/// user's is refused with ELOOP, as syscall.c:560 refuses it.
#[test]
fn an_override_armed_in_a_consumer_crate_changes_the_walk() {
    let temp = tempfile::tempdir().expect("tempdir");
    let real = temp.path().join("real");
    std::fs::create_dir(&real).expect("real dir");
    std::fs::write(real.join("f"), b"DATA\n").expect("write");
    std::os::unix::fs::symlink(&real, temp.path().join("alt")).expect("symlink");
    let through_link = temp.path().join("alt").join("f");

    let mut followed = String::new();
    fast_io::operator_open_basis_read(&through_link)
        .expect("the operator's own parent symlink is followed")
        .read_to_string(&mut followed)
        .expect("read");
    assert_eq!(followed, "DATA\n");

    let refused = {
        let _owner = fast_io::test_seams::judge_symlink_owner("alt", foreign_uid());
        fast_io::operator_open_basis_read(&through_link)
    };
    let error = refused.expect_err("a foreign-owned parent symlink must be refused");
    assert_eq!(
        error.raw_os_error(),
        Some(rustix::io::Errno::LOOP.raw_os_error())
    );
}
