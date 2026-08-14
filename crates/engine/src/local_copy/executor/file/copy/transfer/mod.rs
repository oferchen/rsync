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
    /// Whether to preserve extended attributes.
    ///
    /// On Unix this is POSIX extended attributes; on Windows it maps `-X`
    /// onto NTFS Alternate Data Streams so the `CopyFileExW` fast path can
    /// decide whether to keep or strip the streams the kernel copied.
    #[cfg(all(any(unix, windows), feature = "xattr"))]
    pub preserve_xattrs: bool,
    /// Whether the destination's extended attributes differed from the
    /// source's before the copy touched them.
    ///
    /// Drives the itemize `x` column. upstream: `generator.c:566-572` sets
    /// `ITEM_REPORT_XATTR` from `xattr_diff()`, never from `preserve_xattrs`
    /// alone, so this is captured once in `copy_file` while the destination is
    /// still untouched and carried down to every record site.
    pub xattrs_changed: bool,
    /// Whether to preserve ACLs (Unix only).
    #[cfg(all(any(unix, windows), feature = "acl"))]
    pub preserve_acls: bool,
}

impl TransferFlags {
    /// Returns whether xattrs preservation is effectively enabled,
    /// accounting for compile-time feature flags.
    ///
    /// The sole caller is `wincopy::reconcile_copied_ads`, itself
    /// `#[cfg(feature = "xattr")]` inside the `#[cfg(target_os = "windows")]`
    /// `wincopy` module: it decides whether to keep or strip the Alternate Data
    /// Streams `CopyFileExW` copied. The gate is that intersection exactly,
    /// because `-D warnings` makes an unused method fatal.
    ///
    /// The itemize `x` column is driven by `xattrs_changed` instead, because
    /// upstream reports it from a comparison rather than from the flag.
    #[cfg(all(target_os = "windows", feature = "xattr"))]
    #[inline]
    pub(super) const fn xattrs_enabled(self) -> bool {
        self.preserve_xattrs
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
