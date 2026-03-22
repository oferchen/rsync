//! Directory creation logic - batch and incremental modes.
//!
//! Handles `create_directories` (parallel metadata application),
//! `ensure_relative_parents` (for `--relative` paths), and
//! `create_directory_incremental` (single-directory creation during
//! incremental recursion).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};
use metadata::{MetadataOptions, apply_metadata_from_file_entry};
use protocol::acl::AclCache;
use protocol::flist::FileEntry;
use protocol::xattr::XattrList;

use super::FailedDirectories;
use crate::receiver::{PARALLEL_STAT_THRESHOLD, ReceiverContext, apply_acls_from_receiver_cache};

impl ReceiverContext {
    /// Creates directories from the file list, applying metadata in parallel.
    ///
    /// Two-phase approach: directory creation is sequential (cheap, respects
    /// parent-child ordering), metadata application (`chown`/`chmod`/`utimes`)
    /// runs in parallel via tokio `spawn_blocking` + semaphore when above
    /// threshold.
    ///
    /// Returns a list of metadata errors encountered (path, error message).
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:693` - `dry_run` skips all filesystem modifications
    /// - `generator.c:1432-1500` - directory creation and metadata in `recv_generator()`
    pub(in crate::receiver) fn create_directories(
        &self,
        dest_dir: &Path,
        metadata_opts: &MetadataOptions,
        acl_cache: Option<&AclCache>,
    ) -> io::Result<Vec<(PathBuf, String)>> {
        // upstream: receiver.c:693 - dry_run skips all filesystem modifications
        if self.config.flags.dry_run {
            return Ok(Vec::new());
        }

        let dir_entries: Vec<(usize, PathBuf)> = self
            .file_list
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_dir())
            .map(|(idx, entry)| {
                let relative_path = entry.path();
                let dir_path = if relative_path.as_os_str() == "." {
                    dest_dir.to_path_buf()
                } else {
                    dest_dir.join(relative_path)
                };
                (idx, dir_path)
            })
            .collect();

        let mut failed_dir_paths: std::collections::HashSet<PathBuf> =
            std::collections::HashSet::new();
        for (_, dir_path) in &dir_entries {
            if !dir_path.exists() {
                if let Err(e) = fs::create_dir_all(dir_path) {
                    if e.kind() == io::ErrorKind::PermissionDenied {
                        // upstream: receiver.c - permission denied on mkdir is non-fatal,
                        // sets io_error and continues with remaining files.
                        if self.config.flags.verbose && self.config.connection.client_mode {
                            info_log!(
                                Misc,
                                1,
                                "failed to create directory {}: {}",
                                dir_path.display(),
                                e
                            );
                        }
                        failed_dir_paths.insert(dir_path.clone());
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        // Build owned data for parallel metadata application, skipping failed dirs.
        let metadata_opts_clone = metadata_opts.clone();
        let entry_snapshots: Vec<(PathBuf, FileEntry, Option<XattrList>)> = dir_entries
            .into_iter()
            .filter(|(_, dir_path)| !failed_dir_paths.contains(dir_path))
            .map(|(idx, dir_path)| {
                let entry = &self.file_list[idx];
                let xattr_list = self.resolve_xattr_list(entry);
                (dir_path, entry.clone(), xattr_list)
            })
            .collect();
        let dir_creation_errors: Vec<(PathBuf, String)> = failed_dir_paths
            .into_iter()
            .map(|p| {
                let msg = format!(
                    "failed to create directory {}: Permission denied",
                    p.display()
                );
                (p, msg)
            })
            .collect();

        let acl_cache_clone = acl_cache.cloned();
        let results = crate::parallel_io::map_blocking(
            entry_snapshots,
            PARALLEL_STAT_THRESHOLD,
            move |(dir_path, entry, xattr_list)| {
                if let Err(e) =
                    apply_metadata_from_file_entry(&dir_path, &entry, &metadata_opts_clone)
                {
                    return Some((dir_path, e.to_string()));
                }
                // Apply cached ACLs after metadata
                if let Err(e) = apply_acls_from_receiver_cache(
                    &dir_path,
                    &entry,
                    acl_cache_clone.as_ref(),
                    true, // directories always follow symlinks
                ) {
                    return Some((dir_path, e.to_string()));
                }
                // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
                if let Some(ref xattr_list) = xattr_list {
                    if let Err(e) = metadata::apply_xattrs_from_list(&dir_path, xattr_list, true) {
                        return Some((dir_path, e.to_string()));
                    }
                }
                None
            },
        );

        let mut all_errors: Vec<(PathBuf, String)> = results.into_iter().flatten().collect();
        all_errors.extend(dir_creation_errors);
        Ok(all_errors)
    }

    /// Creates implied parent directories for `--relative` path components.
    ///
    /// When `--relative` is active, the file list may contain entries with deep paths
    /// (e.g., `a/b/c/file.txt`). If `--no-implied-dirs` was used, the intermediate
    /// directories (`a/`, `a/b/`, `a/b/c/`) may not appear as explicit directory
    /// entries in the file list. This method ensures all parent directories exist
    /// before files, symlinks, or other entries are processed.
    ///
    /// Uses a set to track already-created paths, avoiding redundant `mkdir` syscalls
    /// when many entries share common parent directories.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1317-1326` - `make_path()` for missing parents when
    ///   `relative_paths && !implied_dirs`
    /// - `generator.c:1472-1475` - retry `mkdir` after `make_path()` when
    ///   `relative_paths` and initial `mkdir` returns `ENOENT`
    pub(in crate::receiver) fn ensure_relative_parents(&self, dest_dir: &Path) {
        if !self.config.flags.relative || self.config.flags.dry_run {
            return;
        }

        let mut created: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

        for entry in &self.file_list {
            let relative_path = entry.path();
            if relative_path.as_os_str() == "." {
                continue;
            }

            // Collect all ancestor directories that need creation.
            // For path "a/b/c/file.txt", we need "a/", "a/b/", "a/b/c/".
            // For directory entry "a/b/c/", we need "a/", "a/b/".
            let target = if entry.is_dir() {
                // For directories, create parents (not the dir itself - that's handled
                // by create_directories / create_directory_incremental).
                match relative_path.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p,
                    _ => continue,
                }
            } else {
                // For files/symlinks/etc., create all parent directories.
                match relative_path.parent() {
                    Some(p) if !p.as_os_str().is_empty() => p,
                    _ => continue,
                }
            };

            // Walk up the path to find the deepest ancestor that needs creation.
            // Build the list of paths to create from shallowest to deepest.
            let mut ancestors_to_create: Vec<PathBuf> = Vec::new();
            let mut current = target;
            loop {
                let abs_path = dest_dir.join(current);
                if created.contains(&abs_path) || abs_path.exists() {
                    break;
                }
                ancestors_to_create.push(abs_path);
                match current.parent() {
                    Some(p) if !p.as_os_str().is_empty() => current = p,
                    _ => break,
                }
            }

            // Create from shallowest to deepest.
            for dir_path in ancestors_to_create.into_iter().rev() {
                if let Err(e) = fs::create_dir(&dir_path) {
                    if e.kind() != io::ErrorKind::AlreadyExists {
                        debug_log!(
                            Recv,
                            1,
                            "failed to create implied parent directory {}: {}",
                            dir_path.display(),
                            e
                        );
                        break;
                    }
                }
                created.insert(dir_path);
            }
        }
    }

    /// Creates a single directory during incremental processing.
    ///
    /// Returns `Ok(None)` on failure or skip (marks dir as failed).
    /// Returns `Ok(Some(true))` when a new directory was created.
    /// Returns `Ok(Some(false))` when an existing directory had metadata applied.
    /// Only returns `Err` for unrecoverable errors.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1432` - `recv_generator()` creates directories
    /// - `generator.c:1472-1475` - retry `mkdir` after `make_path()`
    pub(in crate::receiver) fn create_directory_incremental(
        &self,
        dest_dir: &Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        failed_dirs: &mut FailedDirectories,
        acl_cache: Option<&AclCache>,
    ) -> io::Result<Option<bool>> {
        let relative_path = entry.path();
        let dir_path = if relative_path.as_os_str() == "." {
            dest_dir.to_path_buf()
        } else {
            dest_dir.join(relative_path)
        };

        // Check if parent is under a failed directory
        if let Some(failed_parent) = failed_dirs.failed_ancestor(entry.name()) {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Skip,
                    1,
                    "skipping directory {} (parent {} failed)",
                    entry.name(),
                    failed_parent
                );
            }
            failed_dirs.mark_failed(entry.name());
            return Ok(None);
        }

        // Try to create the directory
        let is_new = !dir_path.exists();
        if is_new {
            if let Err(e) = fs::create_dir_all(&dir_path) {
                if self.config.flags.verbose && self.config.connection.client_mode {
                    info_log!(
                        Misc,
                        1,
                        "failed to create directory {}: {}",
                        dir_path.display(),
                        e
                    );
                }
                failed_dirs.mark_failed(entry.name());
                return Ok(None);
            }
        }

        // Apply metadata (non-fatal errors)
        if let Err(e) = apply_metadata_from_file_entry(&dir_path, entry, metadata_opts) {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Misc,
                    1,
                    "warning: metadata error for {}: {}",
                    dir_path.display(),
                    e
                );
            }
        } else if let Some(ref xattr_list) = self.resolve_xattr_list(entry) {
            // upstream: xattrs.c:set_xattr() - apply xattrs after metadata
            if let Err(e) = metadata::apply_xattrs_from_list(&dir_path, xattr_list, true) {
                if self.config.flags.verbose && self.config.connection.client_mode {
                    info_log!(
                        Misc,
                        1,
                        "warning: xattr error for {}: {}",
                        dir_path.display(),
                        e
                    );
                }
            }
        }

        // Apply cached ACLs after metadata (non-fatal errors)
        if let Err(e) = apply_acls_from_receiver_cache(&dir_path, entry, acl_cache, true) {
            if self.config.flags.verbose && self.config.connection.client_mode {
                info_log!(
                    Misc,
                    1,
                    "warning: ACL error for {}: {}",
                    dir_path.display(),
                    e
                );
            }
        }

        if self.config.flags.verbose && self.config.connection.client_mode {
            if relative_path.as_os_str() == "." {
                info_log!(Name, 1, "./");
            } else {
                info_log!(Name, 1, "{}/", relative_path.display());
            }
        }

        Ok(Some(is_new))
    }
}
