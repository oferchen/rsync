//! `open(2)` flag values for `OpenOptionsExt::custom_flags` and the
//! confined `openat` helpers.
//!
//! Re-exported from `libc` unchanged so consumer crates can spell the exact
//! flag set upstream rsync passes (`syscall.c:2882` `do_open_nofollow()`,
//! `syscall.c:3548` `do_mkstemp_atfd()`) without declaring an FFI crate of their own.

pub use libc::{O_CLOEXEC, O_CREAT, O_EXCL, O_NOFOLLOW, O_WRONLY};

/// Linux-only `O_NOATIME`, used by `--open-noatime`.
///
/// upstream: `syscall.c:2899-2901` in `do_open_nofollow()` (`syscall.c:2882`) -
/// `O_NOATIME` is added only where it is defined.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub use libc::O_NOATIME;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::OpenOptionsExt;

    /// `O_WRONLY | O_CREAT | O_EXCL` must create a fresh file and refuse an
    /// existing one - the temp-file create contract the receiver relies on.
    #[test]
    fn exclusive_create_flags_refuse_an_existing_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("tmp");
        let open = || {
            fs::OpenOptions::new()
                .write(true)
                .custom_flags(O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC)
                .open(&path)
        };
        open().expect("first create");
        let err = open().expect_err("second create must fail");
        assert_eq!(err.raw_os_error(), Some(crate::errno::EEXIST));
    }

    /// `O_NOATIME` is a real open flag on Linux: an owner may open with it.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn noatime_open_succeeds_for_the_owner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, b"x").expect("write");
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_NOATIME)
            .open(&path)
            .expect("owner may open with O_NOATIME");
    }
}
