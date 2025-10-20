#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_walk` provides a deterministic filesystem traversal used by the Rust
//! rsync implementation when constructing file lists. The walker enumerates
//! regular files, directories, and symbolic links while enforcing relative-path
//! constraints so callers cannot accidentally escape the configured root. The
//! implementation keeps ordering stable across platforms by sorting directory
//! entries lexicographically before yielding them, mirroring upstream rsync's
//! behaviour when building transfer lists.
//!
//! # Design
//!
//! - [`WalkBuilder`] configures traversal options such as whether the root entry
//!   should be emitted and if directory symlinks may be followed.
//! - [`Walker`] implements [`Iterator`] and yields [`WalkEntry`] values in
//!   depth-first order. Directory contents are processed before the walker moves
//!   to the next sibling, keeping the sequence deterministic regardless of the
//!   underlying filesystem's iteration order.
//! - [`WalkError`] describes I/O failures encountered while querying metadata or
//!   reading directories. Errors capture the offending path so higher layers can
//!   surface actionable diagnostics.
//!
//! # Invariants
//!
//! - Returned [`WalkEntry`] values always reference paths that reside within the
//!   configured root. Relative paths never contain `..` segments.
//! - Directory entries are yielded exactly once. When symlink following is
//!   enabled, canonical paths are tracked to avoid cycles even if a symlink
//!   points back to an ancestor directory.
//! - Traversal never panics; unexpected filesystem failures are reported via
//!   [`WalkError`].
//!
//! # Errors
//!
//! Traversal emits [`WalkError`] when filesystem metadata cannot be queried or
//! when reading directory contents fails. Callers can downcast to [`io::Error`]
//! using [`WalkError::source`] to inspect the original failure.
//!
//! # Examples
//!
//! Traverse a directory tree and collect the relative paths discovered by the
//! walker. The example creates a temporary tree containing a nested file.
//!
//! ```
//! use rsync_walk::WalkBuilder;
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
//! let walker = WalkBuilder::new(&root).build()?;
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
//! - [`rsync_engine`](https://docs.rs/rsync-engine/latest/rsync_engine/) for the
//!   transfer planning facilities that will eventually consume the walker.
//! - [`rsync_core`](https://docs.rs/rsync-core/latest/rsync_core/) for the
//!   central orchestration facade.

use std::collections::HashSet;
use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Configures a filesystem traversal rooted at a specific path.
#[derive(Clone, Debug)]
pub struct WalkBuilder {
    root: PathBuf,
    follow_symlinks: bool,
    include_root: bool,
}

impl WalkBuilder {
    /// Creates a new builder that will traverse the provided root path.
    #[must_use]
    pub fn new<P: Into<PathBuf>>(root: P) -> Self {
        Self {
            root: root.into(),
            follow_symlinks: false,
            include_root: true,
        }
    }

    /// Configures whether directory symlinks should be traversed.
    ///
    /// The walker always yields the symlink entry itself. When this option is
    /// enabled and the symlink points to a directory, the walker also descends
    /// into the target directory while maintaining the symlink's relative path
    /// in emitted [`WalkEntry`] values. Canonical paths are tracked to prevent
    /// infinite loops.
    #[must_use]
    pub const fn follow_symlinks(mut self, follow: bool) -> Self {
        self.follow_symlinks = follow;
        self
    }

    /// Controls whether the root entry should be included in the output.
    ///
    /// When disabled, traversal starts directly with the root's children.
    #[must_use]
    pub const fn include_root(mut self, include: bool) -> Self {
        self.include_root = include;
        self
    }

    /// Builds a [`Walker`] using the configured options.
    pub fn build(self) -> Result<Walker, WalkError> {
        let root = absolutize(self.root.clone())?;
        let metadata = fs::symlink_metadata(&root)
            .map_err(|error| WalkError::root_metadata(root.clone(), error))?;

        let mut walker = Walker {
            root,
            follow_symlinks: self.follow_symlinks,
            yielded_root: !self.include_root,
            root_metadata: Some(metadata),
            stack: Vec::new(),
            visited: HashSet::new(),
            finished: false,
        };

        if walker
            .root_metadata
            .as_ref()
            .is_some_and(|m| m.file_type().is_dir())
        {
            walker.push_directory(walker.root.clone(), PathBuf::new(), 0)?;
        }

        Ok(walker)
    }
}

/// Depth-first iterator over filesystem entries.
pub struct Walker {
    root: PathBuf,
    follow_symlinks: bool,
    yielded_root: bool,
    root_metadata: Option<fs::Metadata>,
    stack: Vec<DirectoryState>,
    visited: HashSet<PathBuf>,
    finished: bool,
}

impl Walker {
    fn push_directory(
        &mut self,
        fs_path: PathBuf,
        relative_prefix: PathBuf,
        depth: usize,
    ) -> Result<(), WalkError> {
        let canonical = fs::canonicalize(&fs_path)
            .map_err(|error| WalkError::canonicalize(fs_path.clone(), error))?;
        if !self.visited.insert(canonical) {
            return Ok(());
        }

        let state = DirectoryState::new(fs_path, relative_prefix, depth)?;
        self.stack.push(state);
        Ok(())
    }

    fn prepare_entry(
        &mut self,
        full_path: PathBuf,
        relative_path: PathBuf,
        depth: usize,
    ) -> Result<WalkEntry, WalkError> {
        let metadata = fs::symlink_metadata(&full_path)
            .map_err(|error| WalkError::metadata(full_path.clone(), error))?;
        let mut next_state = None;

        if metadata.file_type().is_dir() {
            next_state = Some((full_path.clone(), relative_path.clone(), depth));
        } else if metadata.file_type().is_symlink() && self.follow_symlinks {
            match fs::metadata(&full_path) {
                Ok(target) if target.is_dir() => {
                    let canonical = fs::canonicalize(&full_path)
                        .map_err(|error| WalkError::canonicalize(full_path.clone(), error))?;
                    next_state = Some((canonical, relative_path.clone(), depth));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(WalkError::metadata(full_path.clone(), error));
                }
            }
        }

        if let Some((dir_path, rel_prefix, dir_depth)) = next_state {
            self.push_directory(dir_path, rel_prefix, dir_depth)?;
        }

        Ok(WalkEntry {
            full_path,
            relative_path,
            metadata,
            depth,
            is_root: false,
        })
    }
}

impl Iterator for Walker {
    type Item = Result<WalkEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        if !self.yielded_root {
            self.yielded_root = true;
            if let Some(metadata) = self.root_metadata.take() {
                let entry = WalkEntry {
                    full_path: self.root.clone(),
                    relative_path: PathBuf::new(),
                    metadata,
                    depth: 0,
                    is_root: true,
                };
                return Some(Ok(entry));
            }
        }

        loop {
            let (full_path, relative_path, depth) = {
                let state = match self.stack.last_mut() {
                    Some(state) => state,
                    None => return None,
                };

                if let Some(name) = state.next_name() {
                    let full_path = state.fs_path.join(&name);
                    let relative_path = if state.relative_prefix.as_os_str().is_empty() {
                        PathBuf::from(&name)
                    } else {
                        let mut rel = state.relative_prefix.clone();
                        rel.push(&name);
                        rel
                    };
                    (full_path, relative_path, state.depth + 1)
                } else {
                    self.stack.pop();
                    continue;
                }
            };

            match self.prepare_entry(full_path, relative_path, depth) {
                Ok(entry) => return Some(Ok(entry)),
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct DirectoryState {
    fs_path: PathBuf,
    relative_prefix: PathBuf,
    entries: Vec<OsString>,
    index: usize,
    depth: usize,
}

impl DirectoryState {
    fn new(fs_path: PathBuf, relative_prefix: PathBuf, depth: usize) -> Result<Self, WalkError> {
        let mut entries = Vec::new();
        let read_dir =
            fs::read_dir(&fs_path).map_err(|error| WalkError::read_dir(fs_path.clone(), error))?;
        for entry in read_dir {
            let entry = entry.map_err(|error| WalkError::read_dir_entry(fs_path.clone(), error))?;
            entries.push(entry.file_name());
        }
        entries.sort();

        Ok(Self {
            fs_path,
            relative_prefix,
            entries,
            index: 0,
            depth,
        })
    }

    fn next_name(&mut self) -> Option<OsString> {
        if let Some(name) = self.entries.get(self.index) {
            self.index += 1;
            Some(name.clone())
        } else {
            None
        }
    }
}

/// Result of a filesystem traversal step.
#[derive(Debug)]
pub struct WalkEntry {
    full_path: PathBuf,
    relative_path: PathBuf,
    metadata: fs::Metadata,
    depth: usize,
    is_root: bool,
}

impl WalkEntry {
    /// Returns the absolute path to the filesystem entry.
    #[must_use]
    pub fn full_path(&self) -> &Path {
        &self.full_path
    }

    /// Returns the path relative to the traversal root.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Provides access to the [`fs::Metadata`] captured for the entry.
    #[must_use]
    pub fn metadata(&self) -> &fs::Metadata {
        &self.metadata
    }

    /// Reports the depth of the entry relative to the root (root depth is `0`).
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Indicates whether this entry corresponds to the traversal root.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.is_root
    }
}

/// Error returned when traversal fails.
#[derive(Debug)]
pub struct WalkError {
    kind: WalkErrorKind,
}

impl WalkError {
    fn new(kind: WalkErrorKind) -> Self {
        Self { kind }
    }

    fn root_metadata(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::RootMetadata { path, source })
    }

    fn read_dir(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::ReadDir { path, source })
    }

    fn read_dir_entry(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::ReadDirEntry { path, source })
    }

    fn metadata(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::Metadata { path, source })
    }

    fn canonicalize(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::Canonicalize { path, source })
    }

    /// Returns the specific failure that terminated traversal.
    #[must_use]
    pub fn kind(&self) -> &WalkErrorKind {
        &self.kind
    }
}

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WalkErrorKind::RootMetadata { path, source } => {
                write!(
                    f,
                    "failed to inspect traversal root '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::ReadDir { path, source } => {
                write!(
                    f,
                    "failed to read directory '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::ReadDirEntry { path, source } => {
                write!(
                    f,
                    "failed to read entry in '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::Metadata { path, source } => {
                write!(
                    f,
                    "failed to inspect metadata for '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::Canonicalize { path, source } => {
                write!(f, "failed to canonicalize '{}': {}", path.display(), source)
            }
        }
    }
}

impl Error for WalkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            WalkErrorKind::RootMetadata { source, .. }
            | WalkErrorKind::ReadDir { source, .. }
            | WalkErrorKind::ReadDirEntry { source, .. }
            | WalkErrorKind::Metadata { source, .. }
            | WalkErrorKind::Canonicalize { source, .. } => Some(source),
        }
    }
}

/// Classification of traversal failures.
#[derive(Debug)]
pub enum WalkErrorKind {
    /// Failed to query metadata for the traversal root.
    RootMetadata {
        /// Path that failed to provide metadata.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to read the contents of a directory.
    ReadDir {
        /// Directory whose contents could not be read.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to obtain a directory entry during iteration.
    ReadDirEntry {
        /// Directory containing the problematic entry.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to retrieve metadata for an entry.
    Metadata {
        /// Path whose metadata could not be retrieved.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to canonicalize a directory path while preventing cycles.
    Canonicalize {
        /// Directory path that failed to canonicalize.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
}

fn absolutize(path: PathBuf) -> Result<PathBuf, WalkError> {
    if path.is_absolute() {
        Ok(path)
    } else {
        let cwd = std::env::current_dir()
            .map_err(|error| WalkError::canonicalize(PathBuf::from("."), error))?;
        Ok(cwd.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn collect_relative_paths(mut walker: Walker) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        while let Some(entry) = walker.next() {
            let entry = entry.expect("walker entry");
            if entry.is_root() {
                continue;
            }
            paths.push(entry.relative_path().to_path_buf());
        }
        paths
    }

    #[test]
    fn walk_errors_when_root_missing() {
        let builder = WalkBuilder::new("/nonexistent/path/for/walker");
        let error = match builder.build() {
            Ok(_) => panic!("missing root should fail"),
            Err(error) => error,
        };
        assert!(matches!(error.kind(), WalkErrorKind::RootMetadata { .. }));
    }

    #[test]
    fn walk_single_file_emits_root_entry() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("file.txt");
        fs::write(&file, b"contents").expect("write");

        let mut walker = WalkBuilder::new(&file).build().expect("build walker");
        let entry = walker.next().expect("entry").expect("entry ok");
        assert!(entry.is_root());
        assert!(entry.relative_path().as_os_str().is_empty());
        assert_eq!(entry.full_path(), file);
        assert!(walker.next().is_none());
    }

    #[test]
    fn walk_directory_yields_deterministic_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        fs::create_dir(&root).expect("create root");
        let dir_a = root.join("a");
        let dir_b = root.join("b");
        let file_c = root.join("c.txt");
        fs::create_dir(&dir_a).expect("dir a");
        fs::create_dir(&dir_b).expect("dir b");
        fs::write(dir_a.join("inner.txt"), b"data").expect("write inner");
        fs::write(&file_c, b"data").expect("write file");

        let walker = WalkBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(
            paths,
            vec![
                PathBuf::from("a"),
                PathBuf::from("a/inner.txt"),
                PathBuf::from("b"),
                PathBuf::from("c.txt"),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn walk_does_not_follow_symlink_by_default() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");
        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("inner.txt"), b"data").expect("write inner");
        symlink(&target, root.join("link")).expect("create symlink");

        let walker = WalkBuilder::new(&root).build().expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths, vec![PathBuf::from("link")]);
    }

    #[cfg(unix)]
    #[test]
    fn walk_follows_symlink_when_enabled() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        let target = temp.path().join("target");
        fs::create_dir(&root).expect("create root");
        fs::create_dir(&target).expect("create target");
        fs::write(target.join("inner.txt"), b"data").expect("write inner");
        symlink(&target, root.join("link")).expect("create symlink");

        let walker = WalkBuilder::new(&root)
            .follow_symlinks(true)
            .build()
            .expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(
            paths,
            vec![PathBuf::from("link"), PathBuf::from("link/inner.txt")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn walk_detects_symlink_cycles() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("root");
        fs::create_dir(&root).expect("create root");
        let _ = symlink(&root, root.join("self"));

        let walker = WalkBuilder::new(&root)
            .follow_symlinks(true)
            .build()
            .expect("build walker");
        let paths = collect_relative_paths(walker);
        assert_eq!(paths, vec![PathBuf::from("self")]);
    }
}
