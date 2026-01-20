//! Directory traversal abstractions for rsync file list generation.
//!
//! This module provides a [`DirectoryWalker`] trait that abstracts directory
//! traversal, enabling different implementations (walkdir-based, custom, etc.)
//! to be used interchangeably. The default implementation uses the `walkdir`
//! crate for efficient, configurable directory walking.
//!
//! # Components
//!
//! - [`DirectoryWalker`]: Trait for directory traversal implementations
//! - [`WalkdirWalker`]: Default implementation using the `walkdir` crate
//! - [`FilteredWalker`]: Decorator that applies filter rules with early pruning
//! - [`WalkConfig`]: Builder for traversal configuration options
//! - [`WalkEntry`]: Represents a single entry yielded during traversal
//! - [`WalkError`]: Error type for traversal failures
//!
//! # Upstream Reference
//!
//! The traversal behavior mirrors upstream rsync's `flist.c`:
//! - Entries are yielded in sorted order (byte-wise on Unix, UTF-16 on Windows)
//! - Symlinks are not followed by default (matching `-l` behavior)
//! - The `-x` flag restricts traversal to a single filesystem
//! - Filter rules can prune directories early to avoid unnecessary I/O
//!
//! # Examples
//!
//! ```no_run
//! use engine::walk::{WalkConfig, WalkdirWalker};
//! use std::path::Path;
//!
//! let config = WalkConfig::default()
//!     .follow_symlinks(false)
//!     .one_file_system(true);
//!
//! let walker = WalkdirWalker::new(Path::new("/src"), config);
//! for entry in walker {
//!     match entry {
//!         Ok(e) => println!("{}", e.path().display()),
//!         Err(e) => eprintln!("Error: {}", e),
//!     }
//! }
//! ```

mod config;
mod entry;
mod error;
mod filtered_walker;
mod walkdir_impl;

pub use config::WalkConfig;
pub use entry::WalkEntry;
pub use error::WalkError;
pub use filtered_walker::FilteredWalker;
pub use walkdir_impl::WalkdirWalker;

use std::path::Path;

/// Trait for directory traversal implementations.
///
/// Implementors yield directory entries in a deterministic order suitable
/// for rsync file list generation. The traversal respects configuration
/// options like symlink following and single-filesystem constraints.
///
/// # Implementors
///
/// - [`WalkdirWalker`]: Default implementation using the `walkdir` crate
pub trait DirectoryWalker: Iterator<Item = Result<WalkEntry, WalkError>> {
    /// Returns the root path being traversed.
    fn root(&self) -> &Path;

    /// Returns the configuration used for this traversal.
    fn config(&self) -> &WalkConfig;

    /// Skips the current directory's remaining contents.
    ///
    /// When called during iteration, subsequent entries from the current
    /// directory are skipped. This is useful for implementing filter-based
    /// pruning where an entire subtree should be excluded.
    fn skip_current_dir(&mut self);
}
