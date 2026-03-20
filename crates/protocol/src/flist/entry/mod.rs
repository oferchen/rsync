//! File entry representation for the rsync file list.
//!
//! A file entry contains all metadata needed to synchronize a single filesystem
//! object (regular file, directory, symlink, device, etc.).
//!
//! # Path Interning
//!
//! Many file entries in a transfer share the same parent directory. The `dirname`
//! field stores an `Arc<Path>` that can be shared across entries via
//! [`super::intern::PathInterner`], reducing heap allocations for directory paths.
//! This mirrors upstream rsync's `file_struct.dirname` which points into a shared
//! string pool (upstream: flist.c:f_name()).

mod accessors;
mod constructors;
mod core;
mod extras;
mod file_type;
#[cfg(test)]
mod tests;

pub use self::core::FileEntry;
pub use self::file_type::FileType;
