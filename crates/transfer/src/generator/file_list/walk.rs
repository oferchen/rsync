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

use logging::info_log;

use crate::role_trailer::error_location;

use super::super::GeneratorContext;
use super::super::io_error_flags;
use super::batch_stat::{StatResult, batch_stat_dir_entries};

impl GeneratorContext {
    /// Pre-checks a top-level source entry and walks it if it exists.
    ///
    /// Returns `true` if the entry was processed (exists or was handled as a
    /// mode-0 sentinel), `false` if the caller should skip to the next entry.
    ///
    /// This method applies `--ignore-missing-args` and `--delete-missing-args`
    /// semantics for top-level source paths and `--files-from` entries. The
    /// distinction from recursive children (which use [`walk_path`] directly)
    /// is critical: a missing top-level source is "never existed at flist time"
    /// (upstream `link_stat ... failed`, exit 23), while a missing recursive
    /// child is "vanished during walk" (upstream `file has vanished`, exit 24).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2254-2272` - `link_stat` + `missing_args` handling per source
    pub(in crate::generator) fn try_walk_source_entry(
        &mut self,
        base: &Path,
        path: &Path,
    ) -> io::Result<bool> {
        // upstream: flist.c:2390 - link_stat() once, then pass &st to
        // send_file_name(). Reuse the metadata to avoid a redundant stat
        // inside walk_path_with_metadata.
        match self.resolve_symlink_metadata(path, base) {
            Ok(metadata) => {
                // Path exists - pass pre-resolved metadata directly.
                self.walk_path_with_metadata(base, path.to_path_buf(), metadata, true)?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match self.missing_args_mode() {
                    // upstream: flist.c:2261 - missing_args == 1: silently skip
                    1 => Ok(false),
                    // upstream: flist.c:2254-2258 - missing_args == 2: emit mode-0 sentinel
                    2 => {
                        self.emit_delete_sentinel(base, path)?;
                        Ok(true)
                    }
                    // upstream: flist.c:1810 - default: link_stat failed + IOERR_GENERAL
                    _ => {
                        // FFV-4: emit the correct error message and error flag
                        // for a source that never existed at flist build time.
                        eprintln!(
                            "rsync: [sender] link_stat \"{}\" failed: {} ({})",
                            path.display(),
                            e,
                            e.raw_os_error().unwrap_or(0),
                        );
                        self.add_io_error(io_error_flags::IOERR_GENERAL);
                        Ok(false)
                    }
                }
            }
            Err(e) => {
                // Non-ENOENT error: log as link_stat failure and record.
                self.log_stat_error(path, &e);
                self.record_io_error(&e);
                Ok(false)
            }
        }
    }

    /// Emits a mode-0 sentinel file entry for `--delete-missing-args`.
    ///
    /// The sentinel has `mode == 0`, which the receiver interprets as an
    /// instruction to delete the corresponding destination path. The entry
    /// carries the relative name so the receiver can locate the target.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2254-2258` - `missing_args == 2`: `make_file()` + `file->mode = 0`
    fn emit_delete_sentinel(&mut self, base: &Path, path: &Path) -> io::Result<()> {
        let relative = path.strip_prefix(base).unwrap_or(path).to_path_buf();
        let relative = if relative.as_os_str().is_empty() {
            PathBuf::from(path.file_name().unwrap_or(path.as_os_str()))
        } else {
            relative
        };
        // upstream: mode=0 signals "delete this entry" to the receiver.
        // Create a regular file entry then override mode to 0.
        let mut entry = protocol::flist::FileEntry::new_file(relative, 0, 0);
        entry.set_mode(0);
        self.push_file_item(entry, path.to_path_buf());
        Ok(())
    }

    /// Walks a path with pre-resolved metadata, skipping the initial stat call.
    ///
    /// `is_top_level` is `true` only for the direct source arguments; recursive
    /// descents into directory children always pass `false`. The flag controls
    /// whether the directory entry receives `XMIT_TOP_DIR` (upstream `FLAG_TOP_DIR`).
    ///
    /// This is the inner implementation shared by [`walk_path`] (which resolves
    /// metadata itself) and the batched-stat path (which pre-resolves metadata
    /// for all directory children in parallel before processing them).
    fn walk_path_with_metadata(
        &mut self,
        base: &Path,
        path: PathBuf,
        metadata: std::fs::Metadata,
        is_top_level: bool,
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

        let mut entry = match self.create_entry(&path, relative, &metadata) {
            Ok(e) => e,
            Err(e) => {
                // upstream: flist.c - rsyserr for make_file() failures
                eprintln!(
                    "rsync: [sender] make_file failed for \"{}\": {} ({})",
                    path.display(),
                    e,
                    e.raw_os_error().unwrap_or(0),
                );
                self.add_io_error(io_error_flags::IOERR_GENERAL);
                return Ok(());
            }
        };

        // upstream: flist.c:2287 - top-level source directories carry
        // FLAG_TOP_DIR so delete_in_dir() can scope deletions. Under
        // --relative the directory entry has a non-empty relative name (e.g.
        // "tmp/dbg/src/usr/bin") instead of ".", but it still needs the flag.
        if is_top_level && metadata.is_dir() {
            entry.set_flags(protocol::flist::FileFlags::new(
                protocol::flist::XMIT_TOP_DIR,
                0,
            ));
        }

        // upstream: flist.c:send_file_list() - scan directory before recording entry
        let should_recurse = metadata.is_dir() && self.config.flags.recursive;
        let dir_read = if should_recurse {
            match std::fs::read_dir(&path) {
                Ok(entries) => Some(entries),
                Err(e) => {
                    // upstream: flist.c:1842 - rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)
                    eprintln!(
                        "rsync: [sender] opendir \"{}\" failed: {} ({})",
                        path.display(),
                        e,
                        e.raw_os_error().unwrap_or(0),
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
                // upstream: flist.c:1842 - rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)
                eprintln!(
                    "rsync: [sender] opendir \"{}\" failed: {} ({})",
                    dir_path.display(),
                    e,
                    e.raw_os_error().unwrap_or(0),
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
                    // upstream: flist.c:1888 - rsyserr(FERROR_XFER, errno, "readdir(%s)", ...)
                    eprintln!(
                        "rsync: [sender] readdir(\"{}\"): {} ({})",
                        dir_path.display(),
                        e,
                        e.raw_os_error().unwrap_or(0),
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
        let stat_results = batch_stat_dir_entries(child_paths, follow, &self.parallel_thresholds);

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
                                // upstream: flist.c:217 - INFO_GTE(SYMSAFE, 1)
                                // fires before the target is dereferenced.
                                info_log!(
                                    Symsafe,
                                    1,
                                    "copying unsafe symlink \"{}\" -> \"{}\"",
                                    path.display(),
                                    target.display()
                                );
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

                    self.walk_path_with_metadata(base, path, meta, false)?;
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
            // upstream: flist.c:1289 - rprintf(c, "file has vanished: %s\n", full_fname(...))
            eprintln!("file has vanished: \"{}\"", path.display());
        } else {
            // upstream: flist.c:1810 - rsyserr(FERROR_XFER, errno, "link_stat %s failed", ...)
            eprintln!(
                "rsync: [sender] link_stat \"{}\" failed: {} ({})",
                path.display(),
                e,
                e.raw_os_error().unwrap_or(0),
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
                // upstream: flist.c:217 - INFO_GTE(SYMSAFE, 1) fires before
                // the unsafe symlink is dereferenced into a regular entry.
                info_log!(
                    Symsafe,
                    1,
                    "copying unsafe symlink \"{}\" -> \"{}\"",
                    path.display(),
                    target.display()
                );
                return std::fs::metadata(path);
            }
        }

        Ok(meta)
    }
}

#[cfg(test)]
mod rsyserr_wording_tests {
    //! Pin per-file `rsyserr`-equivalent wording to upstream rsync 3.4.1
    //! `log.c:rsyserr()` byte-for-byte. See task #2174 and
    //! `docs/audits/error-message-verbatim-audit.md` family 4.

    /// Each tuple is (template-with-{path}-marker, expected-rendered-line).
    /// Templates mirror the literal `eprintln!` formats above so a future
    /// refactor that re-inserts the source-location or role-version trailer
    /// will fail these asserts.
    const CASES: &[(&str, &str)] = &[
        // upstream: flist.c:1810 - "link_stat %s failed"
        (
            "rsync: [sender] link_stat \"{path}\" failed: No such file or directory (2)",
            "rsync: [sender] link_stat \"/p\" failed: No such file or directory (2)",
        ),
        // upstream: flist.c:1842 - "opendir %s failed"
        (
            "rsync: [sender] opendir \"{path}\" failed: Permission denied (13)",
            "rsync: [sender] opendir \"/p\" failed: Permission denied (13)",
        ),
        // upstream: flist.c:1888 - "readdir(%s)"
        (
            "rsync: [sender] readdir(\"{path}\"): Input/output error (5)",
            "rsync: [sender] readdir(\"/p\"): Input/output error (5)",
        ),
        // upstream: flist.c (make_file paths) - follows rsyserr() shape
        (
            "rsync: [sender] make_file failed for \"{path}\": Permission denied (13)",
            "rsync: [sender] make_file failed for \"/p\": Permission denied (13)",
        ),
        // upstream: flist.c:1289 / sender.c:358 - "file has vanished: %s" via full_fname()
        ("file has vanished: \"{path}\"", "file has vanished: \"/p\""),
        // upstream: sender.c:362 - "send_files failed to open %s"
        (
            "rsync: [sender] send_files failed to open \"{path}\": Permission denied (13)",
            "rsync: [sender] send_files failed to open \"/p\": Permission denied (13)",
        ),
    ];

    #[test]
    fn rsyserr_wording_matches_upstream_byte_for_byte() {
        for (template, expected) in CASES {
            let rendered = template.replace("{path}", "/p");
            assert_eq!(
                &rendered, expected,
                "template {template:?} did not match upstream wording"
            );
        }
    }
}

#[cfg(test)]
mod symsafe_emission_tests {
    //! Wording tests for `--info=SYMSAFE` producer emissions on the
    //! sender side.
    //!
    //! Upstream rsync 3.4.1 fires `INFO_GTE(SYMSAFE, 1)` at `flist.c:217`
    //! when `--copy-unsafe-links` triggers a dereference. The exact line
    //! emitted (per `rprintf(FINFO, ...)`) is matched byte-for-byte so
    //! interop harnesses that grep for the literal continue to find it.
    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, info_log, init};

    fn init_symsafe_level1() {
        let mut cfg = VerbosityConfig::default();
        cfg.info.symsafe = 1;
        init(cfg);
        let _ = drain_events();
    }

    fn symsafe_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Symsafe,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn copying_unsafe_symlink_wording_matches_upstream() {
        // upstream: flist.c:217 -
        //     rprintf(FINFO, "copying unsafe symlink \"%s\" -> \"%s\"\n",
        //             path, linkbuf);
        init_symsafe_level1();
        let path = std::path::Path::new("src/link");
        let target = std::path::Path::new("/etc/passwd");
        info_log!(
            Symsafe,
            1,
            "copying unsafe symlink \"{}\" -> \"{}\"",
            path.display(),
            target.display()
        );
        let msgs = symsafe_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "copying unsafe symlink \"src/link\" -> \"/etc/passwd\""),
            "missing upstream wording: {msgs:?}"
        );
    }

    #[test]
    fn symsafe_emissions_suppressed_when_disabled() {
        // Default `VerbosityConfig` leaves `info.symsafe == 0`, mirroring
        // upstream's pre-`-v` state. The macro must not synthesise an event.
        init(VerbosityConfig::default());
        let _ = drain_events();
        info_log!(
            Symsafe,
            1,
            "copying unsafe symlink \"{}\" -> \"{}\"",
            "x",
            "y"
        );
        let msgs = symsafe_messages();
        assert!(
            msgs.is_empty(),
            "SYMSAFE emissions must be gated; got: {msgs:?}"
        );
    }
}
