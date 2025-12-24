#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `flist` provides file list generation and traversal for the Rust rsync
//! implementation, mirroring upstream's `flist.c`. The walker enumerates
//! regular files, directories, and symbolic links while enforcing relative-path
//! constraints so callers cannot accidentally escape the configured root. The
//! implementation keeps ordering stable across platforms by sorting directory
//! entries lexicographically before yielding them, mirroring upstream rsync's
//! behaviour when building transfer lists.
//!
//! # Design
//!
//! - [`FileListBuilder`] configures traversal options such as whether the root entry
//!   should be emitted and if directory symlinks may be followed.
//! - [`FileListWalker`] implements [`Iterator`] and yields [`FileListEntry`] values in
//!   depth-first order. Directory contents are processed before the walker moves
//!   to the next sibling, keeping the sequence deterministic regardless of the
//!   underlying filesystem's iteration order.
//! - [`FileListError`] describes I/O failures encountered while querying metadata or
//!   reading directories. Errors capture the offending path so higher layers can
//!   surface actionable diagnostics.
//!
//! # Invariants
//!
//! - Returned [`FileListEntry`] values always reference paths that reside within the
//!   configured root. Relative paths never contain `..` segments.
//! - Directory entries are yielded exactly once. When symlink following is
//!   enabled, canonical paths are tracked to avoid cycles even if a symlink
//!   points back to an ancestor directory.
//! - Traversal never panics; unexpected filesystem failures are reported via
//!   [`FileListError`].
//!
//! # Errors
//!
//! Traversal emits [`FileListError`] when filesystem metadata cannot be queried or
//! when reading directory contents fails. Callers can downcast to
//! [`std::io::Error`] using [`std::error::Error::source`] to inspect the original
//! failure.
//!
//! # Examples
//!
//! Traverse a directory tree and collect the relative paths discovered by the
//! walker. The example creates a temporary tree containing a nested file.
//!
//! ```
//! use flist::FileListBuilder;
//! use std::collections::BTreeSet;
//! use std::fs;
//!
//! # fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! let temp = tempfile::tempdir()?;
//! let root = temp.path().join("src");
//! let nested = root.join("nested");
//! fs::create_dir_all(&nested)?;
//! fs::write(root.join("file.txt"), b"data")?;
//! fs::write(nested.join("more.txt"), b"data")?;
//!
//! let walker = FileListBuilder::new(&root).build()?;
//! let mut seen = BTreeSet::new();
//! for entry in walker {
//!     let entry = entry?;
//!     if entry.is_root() {
//!         continue;
//!     }
//!     seen.insert(entry.relative_path().to_path_buf());
//! }
//!
//! assert!(seen.contains(std::path::Path::new("file.txt")));
//! assert!(seen.contains(std::path::Path::new("nested")));
//! assert!(seen.contains(std::path::Path::new("nested/more.txt")));
//! # Ok(())
//! # }
//! # demo().unwrap();
//! ```
//!
//! # See also
//!
//! - [`engine`](https://docs.rs/engine/latest/engine/) for the
//!   transfer planning facilities that will eventually consume the walker.
//! - [`core`](https://docs.rs/core/latest/core/) for the
//!   central orchestration facade.

mod builder;
mod entry;
mod error;
mod file_list_walker;

#[cfg(test)]
mod tests;

pub use crate::builder::FileListBuilder;
pub use crate::entry::FileListEntry;
pub use crate::error::{FileListError, FileListErrorKind};
pub use crate::file_list_walker::FileListWalker;
