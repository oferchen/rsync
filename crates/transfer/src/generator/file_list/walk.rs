//! Filesystem walking and directory scanning for the generator role.
//!
//! Implements recursive directory traversal, symlink resolution, and
//! filter application during file list construction.
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
                // upstream: flist.c:1286-1294 - log vanished warning or general error
                if e.kind() == io::ErrorKind::NotFound {
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
                self.record_io_error(&e);
                return Ok(());
            }
        };

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

            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => {
                                self.walk_path(base, entry.path())?;
                            }
                            Err(e) => {
                                // upstream: flist.c - rsyserr for readdir() failures
                                eprintln!(
                                    "rsync: readdir \"{}\" failed: {} ({}) {}{}",
                                    path.display(),
                                    e,
                                    e.raw_os_error().unwrap_or(0),
                                    error_location!(),
                                    crate::role_trailer::sender()
                                );
                                self.record_io_error(&e);
                            }
                        }
                    }
                }
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
                }
            }
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
        if let Some(ref filters) = self.filters {
            let is_dir = metadata.is_dir();
            if !filters.allows(&relative, is_dir) {
                return Ok(());
            }
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
        let dir_entries = if should_recurse {
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

        self.push_file_item(entry, path);

        if let Some(entries) = dir_entries {
            for dir_entry in entries {
                match dir_entry {
                    Ok(de) => {
                        self.walk_path(base, de.path())?;
                    }
                    Err(e) => {
                        // upstream: flist.c - rsyserr for readdir() failures
                        eprintln!(
                            "rsync: readdir failed: {} ({}) {}{}",
                            e,
                            e.raw_os_error().unwrap_or(0),
                            error_location!(),
                            crate::role_trailer::sender()
                        );
                        self.record_io_error(&e);
                    }
                }
            }
        }

        Ok(())
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
