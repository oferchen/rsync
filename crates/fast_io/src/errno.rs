//! POSIX errno values for classifying `io::Error::raw_os_error()`.
//!
//! Consumer crates branch on specific errnos (`EXDEV` from a confined walk,
//! `ENOTEMPTY` from `rmdir`, `ETXTBSY` from a rename) the same way upstream
//! rsync tests `errno` after a failed syscall. The values are re-exported
//! from `libc` unchanged so a consumer never declares an FFI crate of its
//! own, per the workspace two-owner dependency rule.

pub use libc::{
    EACCES, EBUSY, EEXIST, EINVAL, EIO, EISDIR, ELOOP, EMLINK, ENAMETOOLONG, ENODATA, ENOENT,
    ENOMEM, ENOSPC, ENOTDIR, ENOTEMPTY, ENOTSUP, EOPNOTSUPP, EPERM, EROFS, ETXTBSY, EXDEV,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io;
    use std::os::unix::fs::{OpenOptionsExt, symlink};

    /// The re-exported values must be the ones the kernel actually reports.
    ///
    /// A consumer compares them against `raw_os_error()`; a value that drifted
    /// from the OS (a hand-written table, a wrong target) would make every
    /// such branch silently dead.
    #[test]
    fn values_match_the_errnos_the_kernel_reports() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("missing");
        let err = fs::File::open(&missing).expect_err("missing file");
        assert_eq!(err.raw_os_error(), Some(ENOENT));

        let file = dir.path().join("file");
        fs::write(&file, b"x").expect("write");
        let err = fs::File::open(file.join("child")).expect_err("file as dir");
        assert_eq!(err.raw_os_error(), Some(ENOTDIR));

        let link = dir.path().join("link");
        symlink(&file, &link).expect("symlink");
        let err = fs::OpenOptions::new()
            .read(true)
            .custom_flags(crate::open_flags::O_NOFOLLOW)
            .open(&link)
            .expect_err("O_NOFOLLOW on a symlink");
        assert_eq!(err.raw_os_error(), Some(ELOOP));

        let err = fs::create_dir(dir.path()).expect_err("existing dir");
        assert_eq!(err.raw_os_error(), Some(EEXIST));

        let err = fs::remove_dir(dir.path()).expect_err("non-empty dir");
        assert!(
            matches!(err.raw_os_error(), Some(ENOTEMPTY) | Some(EEXIST)),
            "{err}"
        );
        assert_eq!(
            io::Error::from_raw_os_error(EXDEV).kind(),
            io::ErrorKind::CrossesDevices
        );
    }
}
