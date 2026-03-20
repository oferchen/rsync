/// Type of filesystem entry.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum FileType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Block device.
    BlockDevice,
    /// Character device.
    CharDevice,
    /// Named pipe (FIFO).
    Fifo,
    /// Unix domain socket.
    Socket,
}

impl FileType {
    /// Extracts the file type from Unix mode bits.
    ///
    /// The file type is encoded in the upper 4 bits of the mode (S_IFMT mask).
    pub const fn from_mode(mode: u32) -> Option<Self> {
        // S_IFMT = 0o170000
        match mode & 0o170000 {
            0o100000 => Some(Self::Regular),     // S_IFREG
            0o040000 => Some(Self::Directory),   // S_IFDIR
            0o120000 => Some(Self::Symlink),     // S_IFLNK
            0o060000 => Some(Self::BlockDevice), // S_IFBLK
            0o020000 => Some(Self::CharDevice),  // S_IFCHR
            0o010000 => Some(Self::Fifo),        // S_IFIFO
            0o140000 => Some(Self::Socket),      // S_IFSOCK
            _ => None,
        }
    }

    /// Returns the mode bits for this file type.
    #[must_use]
    pub const fn to_mode_bits(self) -> u32 {
        match self {
            Self::Regular => 0o100000,
            Self::Directory => 0o040000,
            Self::Symlink => 0o120000,
            Self::BlockDevice => 0o060000,
            Self::CharDevice => 0o020000,
            Self::Fifo => 0o010000,
            Self::Socket => 0o140000,
        }
    }

    /// Returns true if this type represents a regular file.
    #[must_use]
    pub const fn is_regular(self) -> bool {
        matches!(self, Self::Regular)
    }

    /// Returns true if this type represents a directory.
    #[must_use]
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// Returns true if this type represents a symbolic link.
    #[must_use]
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }

    /// Returns true if this type represents a device (block or character).
    #[must_use]
    pub const fn is_device(self) -> bool {
        matches!(self, Self::BlockDevice | Self::CharDevice)
    }
}
