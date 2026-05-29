//! Flat file-list backing store (RSS-A.5).
//!
//! This module is the phase-1 step of the flat backing-store design in
//! `docs/design/flat-flist-representation.md`: it defines the fixed-size
//! [`FileEntryHeader`] node that a contiguous header array will hold, plus
//! the placeholder handle types it references.
//!
//! It is entirely additive and unwired. Nothing in production references
//! these types; the legacy `Vec<FileEntry>` path is untouched. The 4-byte
//! path interner that backs [`PathHandle`] is [`PathArena`] (RSS-A.5.c);
//! threading `FlatFileList` through the sort, filter, transfer, and engine
//! consumers is RSS-A.6+. All of that remains gated on RSS-2 allocation
//! profiling per the design's validation gate.

mod extras;
mod header;
mod intern;

#[cfg(test)]
mod tests;

pub use extras::{
    EXTRA_ACL, EXTRA_ATIME, EXTRA_ATIME_NSEC, EXTRA_CHECKSUM, EXTRA_CRTIME, EXTRA_DEF_ACL,
    EXTRA_GROUP_NAME, EXTRA_HARDLINK, EXTRA_LINK_TARGET, EXTRA_RDEV, EXTRA_USER_NAME, EXTRA_XATTR,
    ExtrasArena, ExtrasError, FlatExtras,
};
pub use header::{
    ExtrasRef, FileEntryHeader, PRESENT_CONTENT_DIR, PRESENT_GID, PRESENT_LENGTH64,
    PRESENT_MTIME_NSEC, PRESENT_UID, PathHandle,
};
pub use intern::PathArena;
