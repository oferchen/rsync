//! Filesystem walking and directory scanning for the generator role.
//!
//! Implements recursive directory traversal, symlink resolution, and
//! filter application during file list construction. Directory children
//! are batch-stat'd in parallel via [`super::batch_stat`] when the entry
//! count exceeds the parallel threshold.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_file_list()` - recursive directory scanning
//! - `flist.c:readlink_stat()` - symlink resolution modes

use std::io;
use std::path::{Path, PathBuf};

use crate::role_trailer::error_location;

use super::super::GeneratorContext;
use super::super::io_error_flags;
use super::batch_stat::{StatResult, batch_stat_dir_entries};

impl GeneratorContext {
    /// Recursively walks a path and adds entries to the file list.
    ///
    /// # Upstream Reference
    ///
    /// When the source path is a directory ending with '/', upstream rsync includes
    /// the directory itself as "." entry in the file list. This allows the receiver
    /// to create the destination directory and properly set its attributes.
    ///
    /// See flist.c:send_file_list() which adds "." for the top-level directory.
    pub(in crate::generator) fn walk_path(&mut self, base: &Path, path: PathBuf) -> io::Result<()> {
        // upstream: flist.c:readlink_stat() - resolve symlinks based on flags:
        // --copy-links: follow ALL symlinks (stat instead of lstat)
        // --copy-unsafe-links: follow only UNSAFE symlinks (stat if target escapes tree)
        // otherwise: use lstat (preserve symlinks as-is)
        let metadata = match self.resolve_symlink_metadata(&path, base) {
            Ok(m) => m,
            Err(e) => {
                self.log_stat_error(&path, &e);
                self.record_io_error(&e);
                return Ok(());
            }
        };

        self.walk_path_with_metadata(base, path, metadata)
    }

    /// Walks a path with pre-resolved metadata, skipping the initial stat call.
    ///
    /// This is the inner implementation shared by [`walk_path`] (which resolves
    /// metadata itself) and the batched-stat path (which pre-resolves metadata
    /// for all directory children in parallel before processing them).
    fn walk_path_with_metadata(
        &mut self,
        base: &Path,
        path: PathBuf,
        metadata: std::fs::Metadata,
    ) -> io::Result<()> {
        let relative = path.strip_prefix(base).unwrap_or(&path).to_path_buf();

        // upstream: flist.c:2287 - always emit "." with XMIT_TOP_DIR for the
        // root transfer directory. Enables delete_in_dir() when --delete is active.
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            let mut dot_entry = self.create_entry(&path, PathBuf::from("."), &metadata)?;
            dot_entry.set_flags(protocol::flist::FileFlags::new(
                protocol::flist::XMIT_TOP_DIR,
                0,
            ));
            self.push_file_item(dot_entry, path.clone());

            // upstream: exclude.c:push_local_filters() - read per-directory
            // merge files when entering the root transfer directory.
            let guard = self.filter_chain.enter_directory(&path).map_err(|e| {
                io::Error::other(format!(
                    "filter chain error in \"{}\": {e} {}{}",
                    path.display(),
                    error_location!(),
                    crate::role_trailer::sender()
                ))
            })?;

            self.scan_directory_batched(base, &path)?;

            // upstream: exclude.c:pop_local_filters() - restore filter state
            self.filter_chain.leave_directory(guard);
            return Ok(());
        }

        // upstream: flist.c:send_file_name() - skip unsupported file types
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            let ft = metadata.file_type();
            if (ft.is_block_device() || ft.is_char_device()) && !self.config.flags.devices {
                return Ok(());
            }
            if (ft.is_fifo() || ft.is_socket()) && !self.config.flags.specials {
                return Ok(());
            }
        }

        // upstream: flist.c:1332 - is_excluded() applied during make_file()
        // FilterChain evaluates per-directory scoped rules (innermost first)
        // then global rules. If no rules are configured, allows() returns true.
        if !self.filter_chain.allows(&relative, metadata.is_dir()) {
            return Ok(());
        }

        // upstream: generator.c:1547 - skip unsafe symlinks when --safe-links.
        // Sender-side filtering ensures unsafe symlinks never reach the receiver,
        // matching the belt-and-suspenders approach for daemon push interop.
        if self.config.flags.safe_links && metadata.file_type().is_symlink() {
            if let Ok(target) = std::fs::read_link(&path) {
                if super::super::super::symlink_safety::is_unsafe_symlink(
                    target.as_os_str(),
                    &relative,
                ) {
                    return Ok(());
                }
            }
        }

        let entry = match self.create_entry(&path, relative, &metadata) {
            Ok(e) => e,
            Err(e) => {
                // upstream: flist.c - rsyserr for make_file() failures
                eprintln!(
                    "rsync: make_file failed for \"{}\": {} ({}) {}{}",
                    path.display(),
                    e,
                    e.raw_os_error().unwrap_or(0),
                    error_location!(),
                    crate::role_trailer::sender()
                );
                self.add_io_error(io_error_flags::IOERR_GENERAL);
                return Ok(());
            }
        };

        // upstream: flist.c:send_file_list() - scan directory before recording entry
        let should_recurse = metadata.is_dir() && self.config.flags.recursive;
        let dir_read = if should_recurse {
            match std::fs::read_dir(&path) {
                Ok(entries) => Some(entries),
                Err(e) => {
                    // upstream: flist.c - rsyserr for opendir() failures
                    eprintln!(
                        "rsync: opendir \"{}\" failed: {} ({}) {}{}",
                        path.display(),
                        e,
                        e.raw_os_error().unwrap_or(0),
                        error_location!(),
                        crate::role_trailer::sender()
                    );
                    self.record_io_error(&e);
                    None
                }
            }
        } else {
            None
        };

        // Keep a clone of the path before moving it into the file list,
        // needed for enter_directory() if this is a directory we'll recurse into.
        let dir_path = if dir_read.is_some() {
            Some(path.clone())
        } else {
            None
        };

        self.push_file_item(entry, path);

        if let Some(entries) = dir_read {
            // Safety: dir_path is always Some when dir_read is Some
            let dir_path = dir_path.unwrap();

            // upstream: exclude.c:push_local_filters() - read per-directory
            // merge files when entering a subdirectory during recursive walk.
            let guard = self.filter_chain.enter_directory(&dir_path).map_err(|e| {
                io::Error::other(format!(
                    "filter chain error in \"{}\": {e} {}{}",
                    dir_path.display(),
                    error_location!(),
                    crate::role_trailer::sender()
                ))
            })?;

            // Collect directory entries, then batch-stat and process
            self.process_dir_entries_batched(base, &dir_path, entries)?;

            // upstream: exclude.c:pop_local_filters() - restore filter state
            self.filter_chain.leave_directory(guard);
        }

        Ok(())
    }

    /// Reads a directory and batch-stats its children before recursive processing.
    ///
    /// Collects all `DirEntry` paths from `read_dir()`, resolves their metadata
    /// in parallel via [`batch_stat_dir_entries`], then processes each child
    /// through [`walk_path_with_metadata`]. Entries whose stat fails are logged
    /// and recorded as I/O errors without aborting the traversal.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    fn scan_directory_batched(&mut self, base: &Path, dir_path: &Path) -> io::Result<()> {
        match std::fs::read_dir(dir_path) {
            Ok(entries) => self.process_dir_entries_batched(base, dir_path, entries),
            Err(e) => {
                // upstream: flist.c - rsyserr for opendir() failures
                eprintln!(
                    "rsync: opendir \"{}\" failed: {} ({}) {}{}",
                    dir_path.display(),
                    e,
                    e.raw_os_error().unwrap_or(0),
                    error_location!(),
                    crate::role_trailer::sender()
                );
                self.record_io_error(&e);
                Ok(())
            }
        }
    }

    /// Collects paths from a `ReadDir` iterator, batch-stats them, and recurses.
    ///
    /// For entries where `--copy-unsafe-links` requires re-stat (symlinks escaping
    /// the transfer tree), the corrected metadata is resolved after the batch.
    fn process_dir_entries_batched(
        &mut self,
        base: &Path,
        dir_path: &Path,
        entries: std::fs::ReadDir,
    ) -> io::Result<()> {
        // Phase 1: collect child paths from readdir
        let mut child_paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(de) => child_paths.push(de.path()),
                Err(e) => {
                    // upstream: flist.c - rsyserr for readdir() failures
                    eprintln!(
                        "rsync: readdir \"{}\" failed: {} ({}) {}{}",
                        dir_path.display(),
                        e,
                        e.raw_os_error().unwrap_or(0),
                        error_location!(),
                        crate::role_trailer::sender()
                    );
                    self.record_io_error(&e);
                }
            }
        }

        if child_paths.is_empty() {
            return Ok(());
        }

        // Phase 2: determine stat mode and batch-resolve metadata.
        // --copy-links: follow all symlinks (fs::metadata)
        // default: lstat (fs::symlink_metadata)
        // --copy-unsafe-links needs post-batch fixup for unsafe symlinks
        let follow = self.config.flags.copy_links;
        let stat_results = batch_stat_dir_entries(child_paths, follow);

        // Phase 3: process each (path, metadata) pair
        for result in stat_results {
            let StatResult { path, metadata } = result;
            match metadata {
                Ok(mut meta) => {
                    // upstream: flist.c:215 - follow unsafe symlinks when
                    // --copy-unsafe-links. The batch used lstat, so we need
                    // to re-stat symlinks whose target escapes the tree.
                    if !follow
                        && self.config.flags.copy_unsafe_links
                        && meta.file_type().is_symlink()
                    {
                        if let Ok(target) = std::fs::read_link(&path) {
                            let relative = path.strip_prefix(base).unwrap_or(&path);
                            if super::super::super::symlink_safety::is_unsafe_symlink(
                                target.as_os_str(),
                                relative,
                            ) {
                                match std::fs::metadata(&path) {
                                    Ok(followed) => meta = followed,
                                    Err(e) => {
                                        self.log_stat_error(&path, &e);
                                        self.record_io_error(&e);
                                        continue;
                                    }
                                }
                            }
                        }
                    }

                    self.walk_path_with_metadata(base, path, meta)?;
                }
                Err(e) => {
                    self.log_stat_error(&path, &e);
                    self.record_io_error(&e);
                }
            }
        }

        Ok(())
    }

    /// Logs a stat failure with the appropriate upstream error format.
    ///
    /// Distinguishes between vanished files (ENOENT) and general stat errors,
    /// matching upstream `flist.c:1286-1294` error reporting.
    fn log_stat_error(&self, path: &Path, e: &io::Error) {
        if e.kind() == io::ErrorKind::NotFound {
            // upstream: flist.c:1286-1294 - log vanished warning
            eprintln!(
                "file has vanished: {} {}{}",
                path.display(),
                error_location!(),
                crate::role_trailer::generator()
            );
        } else {
            // upstream: flist.c:1290 - rsyserr(FERROR_XFER, ...) for non-ENOENT
            eprintln!(
                "rsync: link_stat \"{}\" failed: {} ({}) {}{}",
                path.display(),
                e,
                e.raw_os_error().unwrap_or(0),
                error_location!(),
                crate::role_trailer::sender()
            );
        }
    }

    /// Resolves symlink metadata following upstream `flist.c:readlink_stat()`.
    ///
    /// Three modes of symlink resolution:
    /// - `--copy-links`: follow ALL symlinks (stat instead of lstat)
    /// - `--copy-unsafe-links`: follow only symlinks whose target escapes
    ///   the transfer tree (converting them to regular files)
    /// - Default: use lstat (preserve symlinks as symlinks)
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:205-232` - `readlink_stat()`
    /// - `flist.c:215` - `copy_unsafe_links && unsafe_symlink(linkbuf, path)`
    pub(in crate::generator) fn resolve_symlink_metadata(
        &self,
        path: &Path,
        base: &Path,
    ) -> io::Result<std::fs::Metadata> {
        if self.config.flags.copy_links {
            return std::fs::metadata(path);
        }

        let meta = std::fs::symlink_metadata(path)?;

        // upstream: flist.c:215 - follow unsafe symlinks when --copy-unsafe-links
        if self.config.flags.copy_unsafe_links && meta.file_type().is_symlink() {
            let target = std::fs::read_link(path)?;
            let relative = path.strip_prefix(base).unwrap_or(path);
            if super::super::super::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative)
            {
                return std::fs::metadata(path);
            }
        }

        Ok(meta)
    }
}
