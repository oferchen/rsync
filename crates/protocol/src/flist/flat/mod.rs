//! Flat file-list backing store (RSS-A.5/A.6).
//!
//! This module implements the flat backing-store design from
//! `docs/design/flat-flist-representation.md`. It defines the fixed-size
//! [`FileEntryHeader`] node, the [`PathArena`] string interner (RSS-A.5.c),
//! the [`ExtrasArena`] blob arena for length-prefixed optional metadata
//! tails (RSS-A.5.b), and the top-level [`FlatFileList`] container that
//! owns all three stores.
//!
//! [`DualFileList`](super::DualFileList) wires the builder path: every
//! [`FileEntry`](super::FileEntry) pushed through `DualFileList::push` is
//! converted to a `FileEntryHeader` + `FlatExtras` and appended via
//! [`FlatFileList::push_with_extras`]. The legacy `Vec<FileEntry>` path
//! remains untouched and runs in parallel for migration safety.

mod extras;
mod flist;
mod header;
mod intern;

#[cfg(test)]
mod tests;

pub use extras::{
    EXTRA_ACL, EXTRA_ATIME, EXTRA_ATIME_NSEC, EXTRA_CHECKSUM, EXTRA_CRTIME, EXTRA_DEF_ACL,
    EXTRA_GROUP_NAME, EXTRA_HARDLINK, EXTRA_LINK_TARGET, EXTRA_RDEV, EXTRA_USER_NAME, EXTRA_XATTR,
    ExtrasArena, ExtrasError, FlatExtras,
};
pub use flist::{FlatFileEntry, FlatFileList};
pub use header::{
    ExtrasRef, FileEntryHeader, PRESENT_CONTENT_DIR, PRESENT_GID, PRESENT_LENGTH64,
    PRESENT_MTIME_NSEC, PRESENT_UID, PathHandle,
};
pub use intern::PathArena;
