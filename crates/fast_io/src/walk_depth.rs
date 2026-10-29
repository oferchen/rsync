//! The depth ceiling of upstream's confined path resolver.
//!
//! Upstream resolves every confined path through a stack of directory
//! descriptors, one per descended component, and the stack has a fixed size.
//! A walk that would hold more refuses with `ENOMEM`, so a transfer stops at
//! the same directory depth on every role that resolves through it: the
//! sender's directory scan and the receiver's per-file operations. oc mirrors
//! the ceiling, not just the walk, so a deep tree transfers the same files and
//! reports the same errors as rsync 3.5.1.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/syscall.c:2940` `#define DS_MAXDEPTH 1024`
//! - `rsync-3.5.1/syscall.c:3004-3010` `ds_push()` - closes the new descriptor
//!   and fails with `ENOMEM` once `top + 1 >= DS_MAXDEPTH`.

use std::io;

/// Directory levels a confined walk may hold below its anchor, counting the
/// anchor's own slot.
///
/// upstream: `rsync-3.5.1/syscall.c:2940` `DS_MAXDEPTH`.
pub const SECURE_WALK_MAX_DEPTH: usize = 1024;

/// Refuses a walk that descends through `descents` directories, as upstream's
/// `ds_push()` does.
///
/// The anchor occupies the first slot, so at most `SECURE_WALK_MAX_DEPTH - 1`
/// directories can be descended. Opening a directory through a followed walk
/// descends into the directory itself; opening a file, or a directory with
/// `O_NOFOLLOW`, descends only through its parents.
///
/// # Errors
///
/// `ENOMEM`, which upstream reports as "Cannot allocate memory", when
/// `descents` reaches [`SECURE_WALK_MAX_DEPTH`].
pub fn check_secure_walk_depth(descents: usize) -> io::Result<()> {
    if descents >= SECURE_WALK_MAX_DEPTH {
        Err(enomem())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn enomem() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOMEM)
}

#[cfg(not(unix))]
fn enomem() -> io::Error {
    io::Error::from(io::ErrorKind::OutOfMemory)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The last level upstream can still descend into is accepted; the next
    /// one is refused. Both sides pin the boundary, so an off-by-one in
    /// either direction fails here.
    #[test]
    fn the_ceiling_admits_1023_descents_and_refuses_the_1024th() {
        assert!(check_secure_walk_depth(0).is_ok());
        assert!(check_secure_walk_depth(SECURE_WALK_MAX_DEPTH - 1).is_ok());
        let err = check_secure_walk_depth(SECURE_WALK_MAX_DEPTH).expect_err("past the ceiling");
        #[cfg(unix)]
        assert_eq!(err.raw_os_error(), Some(libc::ENOMEM));
        #[cfg(not(unix))]
        assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    }

    /// The value is upstream's, not a tunable.
    #[test]
    fn the_ceiling_is_upstreams_ds_maxdepth() {
        assert_eq!(SECURE_WALK_MAX_DEPTH, 1024);
    }
}
