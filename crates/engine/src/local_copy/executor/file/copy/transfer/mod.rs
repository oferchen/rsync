//! File transfer orchestration for the local copy executor.
//!
//! This module coordinates the transfer of a single file from source to
//! destination, encompassing skip detection, append-mode resume, delta
//! transfer, write strategy selection, and post-transfer metadata
//! finalization.
//!
//! # Submodules
//!
//! - `execute` - main transfer pipeline (`execute_transfer`)
//! - `finalize` - guard commit and metadata application
//! - `open` - source file opening with `O_NOATIME` support
//! - `special` - non-regular files copied as empty regular files
//! - `write_strategy` - write strategy selection (append, inplace, direct, temp-file)

mod execute;
mod finalize;
mod open;
mod special;
mod write_strategy;

pub(super) use execute::execute_transfer;
#[cfg(test)]
pub(crate) use open::take_fsync_call_count;

/// Boolean flags controlling file transfer behavior.
///
/// Groups the boolean parameters that govern write strategy, comparison mode,
/// and platform-specific preservation options for `execute_transfer`.
#[derive(Clone, Copy, Debug)]
pub(super) struct TransferFlags {
    /// Whether append mode is allowed for existing files.
    pub append_allowed: bool,
    /// Whether to verify appended data matches the source.
    pub append_verify: bool,
    /// Whether to always transfer the entire file (no delta).
    pub whole_file_enabled: bool,
    /// Whether to update the file in place (no temp file).
    pub inplace_enabled: bool,
    /// Whether to keep partial transfers on interruption.
    pub partial_enabled: bool,
    /// Whether to use sparse writes for zero-filled regions.
    pub use_sparse_writes: bool,
    /// Whether to compress data during transfer.
    pub compress_enabled: bool,
    /// Whether to compare files by size only.
    pub size_only_enabled: bool,
    /// Whether to ignore modification times when comparing.
    pub ignore_times_enabled: bool,
    /// Whether to use checksums for comparison.
    pub checksum_enabled: bool,
    /// Whether to preserve extended attributes (Unix only).
    #[cfg(all(unix, feature = "xattr"))]
    pub preserve_xattrs: bool,
    /// Whether to preserve ACLs (Unix only).
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub preserve_acls: bool,
}

impl TransferFlags {
    /// Returns whether xattrs preservation is effectively enabled,
    /// accounting for compile-time feature flags.
    #[inline]
    pub(super) const fn xattrs_enabled(self) -> bool {
        #[cfg(all(unix, feature = "xattr"))]
        {
            self.preserve_xattrs
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            false
        }
    }

    /// Returns whether ACL preservation is effectively enabled,
    /// accounting for compile-time feature flags.
    #[inline]
    pub(super) const fn acls_enabled(self) -> bool {
        #[cfg(all(any(unix, windows), feature = "acl"))]
        {
            self.preserve_acls
        }
        #[cfg(not(all(any(unix, windows), feature = "acl")))]
        {
            false
        }
    }
}
