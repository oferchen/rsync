#![allow(clippy::module_name_repetitions)]

//! File list (flist) encoding and decoding for the rsync protocol.
//!
//! When rsync transfers files, it first exchanges a file list containing metadata
//! for all entries to be synchronized. This module implements the wire format for
//! reading and writing these file list entries.
//!
//! # Wire Format Overview
//!
//! Each file entry is encoded as:
//! 1. A flags byte (or extended flags for protocol 28+) indicating which fields follow
//! 2. Path bytes with common prefix compression (reuses prefix from previous entry)
//! 3. File size (varint encoded)
//! 4. Modification time (conditionally, based on flags)
//! 5. Mode bits (conditionally, based on flags)
//! 6. UID/GID (conditionally, based on flags and protocol options)
//! 7. Device/rdev for special files (conditionally)
//! 8. Symlink target (for symlinks)
//!
//! # Example
//!
//! ```
//! use protocol::flist::{FileEntry, FileType};
//!
//! let entry = FileEntry::new_file("test.txt".into(), 1024, 0o644);
//! assert_eq!(entry.name(), "test.txt");
//! assert_eq!(entry.size(), 1024);
//! assert_eq!(entry.file_type(), FileType::Regular);
//! ```

mod entry;
mod flags;
mod hardlink;
mod incremental;
mod macros;
mod read;
mod sort;
mod state;
mod write;

pub use entry::{FileEntry, FileType};
pub use flags::{FileFlags, XMIT_SAME_RDEV_PRE28};
pub use hardlink::{DevIno, HardlinkEntry, HardlinkLookup, HardlinkTable};
pub use read::{FileListReader, read_file_entry};
pub use sort::{
    CleanResult, compare_file_entries, flist_clean, sort_and_clean_file_list, sort_file_list,
};
pub use state::{FileListCompressionState, FileListStats};
pub use write::{FileListWriter, write_file_entry};
pub use incremental::{
    IncrementalFileList, IncrementalFileListBuilder, IncrementalFileListIter, StreamingFileList,
};
