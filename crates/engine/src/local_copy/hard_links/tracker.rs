//! Cross-platform source-side hardlink tracker facade.
//!
//! Dispatches to the platform-specific implementation in [`super::unix`] or
//! [`super::windows`]. The exposed [`HardLinkTracker`] type is identical in
//! API across platforms; only the storage backend differs based on whether
//! the OS exposes inode metadata.

#[cfg(unix)]
pub(crate) use super::unix::HardLinkTracker;

#[cfg(not(unix))]
pub(crate) use super::windows::HardLinkTracker;
