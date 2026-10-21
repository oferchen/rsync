//! Closing a written file with the result of `close(2)` reported.
//!
//! `Drop for File` discards whatever `close` returns, but on NFS, FUSE and
//! other filesystems that defer write-back, `close` is where a failed write
//! first surfaces. Upstream's receiver checks it and aborts:
//!
//! ```c
//! /* receiver.c:1296-1300 - recv_files() */
//! if (close(fd2) < 0) {
//!         rsyserr(FERROR, errno, "close failed on %s", full_fname(fnametmp));
//!         exit_cleanup(RERR_FILEIO);
//! }
//! ```

use std::fs::File;
use std::io;

/// Closes `file` and returns the error `close(2)` reported, if any.
///
/// The descriptor is released either way; on error it must not be closed
/// again, which taking `file` by value guarantees.
///
/// # Errors
///
/// Returns the OS error from `close(2)`, e.g. `EIO` from deferred write-back.
#[cfg(unix)]
pub fn close_file(file: File) -> io::Result<()> {
    nix::unistd::close(file).map_err(io::Error::from)
}

/// Closes `file`.
///
/// `CloseHandle` reports no deferred write-back errors on Windows: a failed
/// write surfaces from `WriteFile` or `FlushFileBuffers`, both of which the
/// caller has already checked, so dropping the handle loses nothing.
///
/// # Errors
///
/// Never fails on this platform.
#[cfg(not(unix))]
pub fn close_file(file: File) -> io::Result<()> {
    drop(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn closing_a_written_file_succeeds_and_keeps_its_bytes() {
        let dir = test_support::create_tempdir();
        let path = dir.path().join("out");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"payload").unwrap();

        close_file(file).expect("a healthy close reports success");

        assert_eq!(std::fs::read(&path).unwrap(), b"payload");
    }
}
