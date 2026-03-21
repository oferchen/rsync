#![deny(unsafe_code)]

//! Implements upstream rsync's --itemize-changes (-i) output format.
//!
//! The itemize format is an 11-character string: `YXcstpoguax`
//!
//! - Position 0 (Y): Update type
//!   - `<` sent to remote
//!   - `>` received from remote
//!   - `c` local change (created)
//!   - `h` hard link
//!   - `.` not updated
//!   - `*` message (e.g., `*deleting`)
//! - Position 1 (X): File type
//!   - `f` regular file
//!   - `d` directory
//!   - `L` symlink
//!   - `D` device (char or block)
//!   - `S` special file (fifo, socket)
//! - Positions 2-10: Attribute changes
//!   - Position 2 (c): checksum differs (or `+` for new file)
//!   - Position 3 (s): size differs
//!   - Position 4 (t): modification time differs (`t`) or set to transfer time (`T`)
//!   - Position 5 (p): permissions differ
//!   - Position 6 (o): owner differs
//!   - Position 7 (g): group differs
//!   - Position 8 (u): reserved for atime/ctime (`u` = atime, `n` = ctime, `b` = both)
//!   - Position 9 (a): ACL differs
//!   - Position 10 (x): extended attributes differ
//!
//! All unchanged attributes show `.` (dot). New files show `+` for all attributes.
//!
//! # Examples
//!
//! ```
//! use cli::ItemizeChange;
//!
//! // New file
//! let change = ItemizeChange::new()
//!     .with_update_type(cli::UpdateType::Received)
//!     .with_file_type(cli::FileType::RegularFile)
//!     .with_new_file(true);
//! assert_eq!(change.format(), ">f+++++++++");
//!
//! // File with checksum and size changed
//! let change = ItemizeChange::new()
//!     .with_update_type(cli::UpdateType::Received)
//!     .with_file_type(cli::FileType::RegularFile)
//!     .with_checksum_changed(true)
//!     .with_size_changed(true);
//! assert_eq!(change.format(), ">fcs.......");
//!
//! // Unchanged file
//! let change = ItemizeChange::new()
//!     .with_update_type(cli::UpdateType::NotUpdated)
//!     .with_file_type(cli::FileType::RegularFile);
//! assert_eq!(change.format(), ".f         ");
//! ```

mod change;
mod format;
mod types;

pub use change::ItemizeChange;
pub use format::format_itemize;
pub use types::{FileType, UpdateType};

#[cfg(test)]
mod tests;
