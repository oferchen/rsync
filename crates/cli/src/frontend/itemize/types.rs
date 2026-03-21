//! Type indicators for the rsync itemize format.
//!
//! Defines the update type (position 0) and file type (position 1) enums
//! used in the 11-character `YXcstpoguax` output string.
//!
//! # Upstream Reference
//!
//! `log.c` - itemize output character mapping.

/// Update type indicator (position 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateType {
    /// `<` - sent to remote
    Sent,
    /// `>` - received from remote
    Received,
    /// `c` - local change (created)
    Created,
    /// `h` - hard link
    HardLink,
    /// `.` - not updated
    NotUpdated,
    /// `*` - message follows (e.g., `*deleting`)
    Message,
}

impl UpdateType {
    /// Returns the character representation for position 0.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::Sent => '<',
            Self::Received => '>',
            Self::Created => 'c',
            Self::HardLink => 'h',
            Self::NotUpdated => '.',
            Self::Message => '*',
        }
    }
}

/// File type indicator (position 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// `f` - regular file
    RegularFile,
    /// `d` - directory
    Directory,
    /// `L` - symlink
    Symlink,
    /// `D` - device (char or block)
    Device,
    /// `S` - special file (fifo, socket)
    Special,
}

impl FileType {
    /// Returns the character representation for position 1.
    #[must_use]
    pub const fn as_char(self) -> char {
        match self {
            Self::RegularFile => 'f',
            Self::Directory => 'd',
            Self::Symlink => 'L',
            Self::Device => 'D',
            Self::Special => 'S',
        }
    }
}
