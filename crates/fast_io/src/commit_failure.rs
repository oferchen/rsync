//! The stage of a batched file commit that failed.
//!
//! A batched writer commits a file in one call that drains its buffered
//! writes and, under `--fsync`, syncs the file. Upstream reports the two
//! stages with different lines and levels - `write failed on %s` (FERROR_XFER,
//! `receiver.c:658-660`) and `fsync failed on %s` (FERROR,
//! `receiver.c:679-681`) - so the caller has to know which one failed. The
//! error is returned unwrapped inside the variant, keeping its OS error code.

use std::io;

/// Which stage of a batched commit failed, carrying that stage's error.
#[derive(Debug)]
pub enum CommitFailure {
    /// Draining the buffered writes into the file failed.
    Write(io::Error),
    /// Syncing the file under `--fsync` failed.
    Fsync(io::Error),
}

impl CommitFailure {
    /// The stage's error, for callers that do not distinguish the stages.
    #[must_use]
    pub fn into_inner(self) -> io::Error {
        match self {
            Self::Write(error) | Self::Fsync(error) => error,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn into_inner_keeps_the_os_error_of_either_stage() {
        let write = CommitFailure::Write(io::Error::from_raw_os_error(28));
        let fsync = CommitFailure::Fsync(io::Error::from_raw_os_error(5));

        assert_eq!(write.into_inner().raw_os_error(), Some(28));
        assert_eq!(fsync.into_inner().raw_os_error(), Some(5));
    }
}
