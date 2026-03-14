//! Directory, symlink, and hardlink creation; extraneous file deletion.
//!
//! Handles filesystem mutations driven by the received file list, including
//! directory creation (batch and incremental), symlink creation, hardlink
//! creation for both protocol 30+ and pre-30 modes, and `--delete` scanning.

use std::fs;
use std::io::{self};
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};
use metadata::{MetadataOptions, apply_metadata_from_file_entry};
use protocol::flist::FileEntry;
use protocol::stats::DeleteStats;
use protocol::xattr::XattrList;

use protocol::acl::AclCache;

use super::{PARALLEL_STAT_THRESHOLD, ReceiverContext, apply_acls_from_receiver_cache};

/// Tracks directories that failed to create.
///
/// Children of failed directories are skipped during incremental processing.
#[derive(Debug, Default)]
pub(super) struct FailedDirectories {
    /// Failed directory paths (normalized, no trailing slash).
    paths: std::collections::HashSet<String>,
}

impl FailedDirectories {
    /// Creates a new empty tracker.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Marks a directory as failed.
    pub(super) fn mark_failed(&mut self, path: &str) {
        self.paths.insert(path.to_string());
    }

    /// Checks if an entry path has a failed ancestor directory.
    ///
    /// Returns the failed ancestor path if found, `None` otherwise.
    pub(super) fn failed_ancestor(&self, entry_path: &str) -> Option<&str> {
        // Check if exact path is failed
        if self.paths.contains(entry_path) {
            return self.paths.get(entry_path).map(|s| s.as_str());
        }

        // Check each parent path component
        let mut check_path = entry_path;
        while let Some(pos) = check_path.rfind('/') {
            check_path = &check_path[..pos];
            if let Some(failed) = self.paths.get(check_path) {
                return Some(failed.as_str());
            }
        }
        None
    }

    /// Returns the number of failed directories.
    #[cfg(test)]
    pub(super) fn count(&self) -> usize {
        self.paths.len()
    }
}

impl ReceiverContext {
    /// Creates directories from the file list, applying metadata in parallel.
    ///
    /// Two-phase approach: directory creation is sequential (cheap, respects
    /// parent-child ordering), metadata application (`chown`/`chmod`/`utimes`)
    /// runs in parallel via tokio `spawn_blocking` + semaphore when above
    /// threshold.
    ///
    /// Returns a list of metadata errors encountered (path, error message).
    pub(super) fn create_directories(
        &self,
        dest_dir: &std::path::Path,
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
    pub(super) fn ensure_relative_parents(&self, dest_dir: &Path) {
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

    /// Deletes extraneous files at the destination that are not in the received file list.
    ///
    /// Groups file list entries by parent directory, then for each destination directory,
    /// scans for entries not present in the source list and removes them. Directories
    /// are removed recursively (depth-first).
    ///
    /// Uses tokio `spawn_blocking` + semaphore for parallel directory scanning when
    /// directory count exceeds threshold. When `max_delete` is set, an atomic counter
    /// enforces the deletion limit across all parallel workers.
    ///
    /// Returns `(stats, limit_exceeded)` where `limit_exceeded` is true when deletions
    /// were stopped due to `--max-delete`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:delete_in_dir()` - scans one directory, removes unlisted entries
    /// - `generator.c:do_delete_pass()` - full tree walk deletion sweep
    /// - `main.c:1367` - `deletion_count >= max_delete` check
    pub(super) fn delete_extraneous_files(
        &self,
        dest_dir: &Path,
    ) -> io::Result<(DeleteStats, bool)> {
        use std::collections::{HashMap, HashSet};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let max_delete = self.config.deletion.max_delete;

        // Build directory -> children map from the file list.
        // Use owned OsString keys so the map can be shared across threads.
        let mut dir_children: HashMap<PathBuf, HashSet<std::ffi::OsString>> = HashMap::new();

        for entry in &self.file_list {
            let relative = entry.path();
            if relative.as_os_str() == "." {
                continue;
            }
            let parent = relative.parent().map_or_else(
                || Path::new(".").to_path_buf(),
                |p| {
                    if p.as_os_str().is_empty() {
                        Path::new(".").to_path_buf()
                    } else {
                        p.to_path_buf()
                    }
                },
            );
            if let Some(name) = relative.file_name() {
                dir_children
                    .entry(parent)
                    .or_default()
                    .insert(name.to_os_string());
            }
        }

        let dirs_to_scan: Vec<PathBuf> = dir_children.keys().cloned().collect();

        // Atomic counter for max_delete enforcement across parallel workers.
        // upstream: main.c:1367 - deletion_count >= max_delete
        let deletions_performed = Arc::new(AtomicU64::new(0));

        // Share directory children map across workers.
        let dir_children = Arc::new(dir_children);
        let dest_dir_owned = dest_dir.to_path_buf();

        let per_dir_results = crate::parallel_io::map_blocking(
            dirs_to_scan,
            PARALLEL_STAT_THRESHOLD,
            move |dir_relative| {
                let dest_path = if dir_relative.as_os_str() == "." {
                    dest_dir_owned.clone()
                } else {
                    dest_dir_owned.join(&dir_relative)
                };

                let keep = match dir_children.get(&dir_relative) {
                    Some(set) => set,
                    None => return DeleteStats::new(),
                };

                let read_dir = match fs::read_dir(&dest_path) {
                    Ok(iter) => iter,
                    Err(_) => return DeleteStats::new(),
                };

                let mut stats = DeleteStats::new();
                for entry in read_dir {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    let name = entry.file_name();
                    if keep.contains(&name) {
                        continue;
                    }

                    // Check max_delete limit before each deletion.
                    if let Some(limit) = max_delete {
                        let current = deletions_performed.load(Ordering::Relaxed);
                        if current >= limit {
                            break;
                        }
                        deletions_performed.fetch_add(1, Ordering::Relaxed);
                    }

                    let path = dest_path.join(&name);
                    let file_type = entry.file_type().ok();
                    let is_dir = file_type.as_ref().is_some_and(|ft| ft.is_dir());
                    let is_symlink = file_type.as_ref().is_some_and(|ft| ft.is_symlink());

                    let result = if is_dir {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };

                    match result {
                        Ok(()) => {
                            if is_dir {
                                info_log!(Del, 1, "deleting directory {}", path.display());
                                stats.dirs += 1;
                            } else if is_symlink {
                                info_log!(Del, 1, "deleting {}", path.display());
                                stats.symlinks += 1;
                            } else {
                                info_log!(Del, 1, "deleting {}", path.display());
                                stats.files += 1;
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => {
                            debug_log!(Del, 1, "failed to delete {}: {}", path.display(), e);
                        }
                    }
                }
                stats
            },
        );

        let mut combined = DeleteStats::new();
        for s in per_dir_results {
            combined.files = combined.files.saturating_add(s.files);
            combined.dirs = combined.dirs.saturating_add(s.dirs);
            combined.symlinks = combined.symlinks.saturating_add(s.symlinks);
            combined.devices = combined.devices.saturating_add(s.devices);
            combined.specials = combined.specials.saturating_add(s.specials);
        }

        // Limit is exceeded when we had candidates beyond the allowed count.
        let total_deletions = u64::from(combined.files)
            + u64::from(combined.dirs)
            + u64::from(combined.symlinks)
            + u64::from(combined.devices)
            + u64::from(combined.specials);
        let limit_exceeded = max_delete.is_some_and(|limit| total_deletions >= limit);

        Ok((combined, limit_exceeded))
    }

    /// Creates symbolic links from the file list entries.
    ///
    /// Iterates through the received file list, finds symlink entries with
    /// `preserve_links` enabled, and creates them on the destination filesystem.
    /// Existing symlinks pointing to the correct target are skipped (quick-check).
    /// Existing files/symlinks at the destination path are removed before creation.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1544` - `if (preserve_links && ftype == FT_SYMLINK)`
    /// - `generator.c:1591` - `atomic_create(file, fname, sl, ...)`
    #[cfg(unix)]
    pub(super) fn create_symlinks(&self, dest_dir: &Path) {
        if !self.config.flags.links || self.config.flags.dry_run {
            return;
        }

        for entry in &self.file_list {
            if !entry.is_symlink() {
                continue;
            }

            let target = match entry.link_target() {
                Some(t) => t,
                None => continue,
            };

            let relative_path = entry.path();

            // upstream: generator.c:1547 - skip unsafe symlinks when --safe-links
            // is set. Check stays here (not in sanitize_file_list) to preserve
            // protocol index alignment with the sender.
            if self.config.flags.safe_links
                && crate::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative_path)
            {
                continue;
            }

            let link_path = dest_dir.join(relative_path);

            // upstream: generator.c:1561 - quick_check_ok(FT_SYMLINK, ...)
            if let Ok(existing_target) = std::fs::read_link(&link_path) {
                if existing_target == *target {
                    continue;
                }
                let _ = std::fs::remove_file(&link_path);
            } else if link_path.symlink_metadata().is_ok() {
                let _ = std::fs::remove_file(&link_path);
            }

            // Ensure parent directory exists for --relative paths.
            // upstream: generator.c:1317-1326 - make_path() for relative_paths
            if let Some(parent) = link_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // upstream: generator.c:1591 - atomic_create() -> do_symlink()
            if let Err(e) = std::os::unix::fs::symlink(target, &link_path) {
                debug_log!(
                    Recv,
                    1,
                    "failed to create symlink {} -> {}: {}",
                    link_path.display(),
                    target.display(),
                    e
                );
            }
        }
    }

    /// No-op on non-Unix platforms where symlinks are not supported.
    #[cfg(not(unix))]
    pub(super) fn create_symlinks(&self, _dest_dir: &Path) {}

    /// Creates hard links for hardlink follower entries after the leader has been transferred.
    ///
    /// In the rsync protocol (30+), hardlinked files are grouped by a shared index.
    /// The first file in each group (the "leader", `hardlink_idx == u32::MAX`) is
    /// transferred normally. Subsequent files ("followers") carry the leader's file
    /// list index in their `hardlink_idx` field and are created as hard links to the
    /// leader's destination path instead of being transferred independently.
    ///
    /// For protocol 28-29, hardlinks are identified by (dev, ino) pairs. This method
    /// handles both cases by building a mapping from hardlink identifiers to leader
    /// destination paths.
    ///
    /// # Upstream Reference
    ///
    /// - `hlink.c:hard_link_check()` - skips followers, links to leader
    /// - `hlink.c:finish_hard_link()` - creates links after leader transfer completes
    /// - `generator.c:1539` - `F_HLINK_NOT_FIRST` check before `hard_link_check()`
    pub(super) fn create_hardlinks(&self, dest_dir: &Path) {
        if !self.config.flags.hard_links || self.config.flags.dry_run {
            return;
        }

        // Protocol 30+: use hardlink_idx to map followers to leaders.
        // Build a map from file list index -> destination path for leaders.
        let mut leader_paths: std::collections::HashMap<u32, PathBuf> =
            std::collections::HashMap::new();

        // First pass: record leader paths keyed by their readdir-order wire NDX
        // (stored in hardlink_idx during receive_file_list before sorting).
        // upstream: flist.c - F_HL_GNUM stores the readdir-order wire NDX of the leader
        for entry in &self.file_list {
            if entry.flags().hlink_first() {
                let leader_gnum = match entry.hardlink_idx() {
                    Some(idx) => idx,
                    None => continue,
                };
                let relative_path = entry.path();
                let dest_path = if relative_path.as_os_str() == "." {
                    dest_dir.to_path_buf()
                } else {
                    dest_dir.join(relative_path)
                };
                leader_paths.insert(leader_gnum, dest_path);
            }
        }

        // Second pass: create hard links for followers (hlinked but NOT hlink_first).
        for entry in &self.file_list {
            if !entry.flags().hlinked() || entry.flags().hlink_first() {
                continue;
            }
            let leader_idx = match entry.hardlink_idx() {
                Some(idx) => idx,
                None => continue,
            };

            let leader_path = match leader_paths.get(&leader_idx) {
                Some(p) => p,
                None => {
                    debug_log!(
                        Recv,
                        1,
                        "hardlink follower {} references unknown leader index {}",
                        entry.name(),
                        leader_idx
                    );
                    continue;
                }
            };

            let relative_path = entry.path();
            let link_path = dest_dir.join(relative_path);

            // Quick-check: if destination already hard-links to the leader, skip.
            if let Ok(link_meta) = fs::symlink_metadata(&link_path) {
                if let Ok(leader_meta) = fs::symlink_metadata(leader_path) {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if link_meta.dev() == leader_meta.dev()
                            && link_meta.ino() == leader_meta.ino()
                        {
                            continue;
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = (link_meta, leader_meta);
                    }
                }
                // Remove existing file so we can create the hard link.
                let _ = fs::remove_file(&link_path);
            }

            // Ensure parent directory exists.
            if let Some(parent) = link_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // upstream: hlink.c:maybe_hard_link() -> atomic_create() -> do_link()
            if let Err(e) = fs::hard_link(leader_path, &link_path) {
                debug_log!(
                    Recv,
                    1,
                    "failed to hard link {} => {}: {}",
                    link_path.display(),
                    leader_path.display(),
                    e
                );
            }
        }

        // Protocol 28-29: use (dev, ino) pairs to identify hardlink groups.
        // Build groups from entries that have hardlink_dev/ino set but no hardlink_idx.
        if self.protocol.as_u8() < 30 {
            self.create_hardlinks_pre30(dest_dir);
        }
    }

    /// Creates hard links for protocol 28-29 using (dev, ino) pairs.
    ///
    /// In protocols before 30, hardlinks are identified by matching (dev, ino)
    /// pairs across file list entries. The first entry with a given (dev, ino) is
    /// the leader; subsequent entries are followers that should be hard-linked.
    fn create_hardlinks_pre30(&self, dest_dir: &Path) {
        use std::collections::HashMap;

        // Map from (dev, ino) -> first file's destination path.
        let mut dev_ino_map: HashMap<(i64, i64), PathBuf> = HashMap::new();

        for entry in &self.file_list {
            if !entry.is_file() {
                continue;
            }

            let dev = match entry.hardlink_dev() {
                Some(d) => d,
                None => continue,
            };
            let ino = match entry.hardlink_ino() {
                Some(i) => i,
                None => continue,
            };

            let relative_path = entry.path();
            let dest_path = dest_dir.join(relative_path);

            match dev_ino_map.entry((dev, ino)) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(dest_path);
                }
                std::collections::hash_map::Entry::Occupied(e) => {
                    let leader_path = e.get();

                    // Quick-check: skip if already linked.
                    if let Ok(link_meta) = fs::symlink_metadata(&dest_path) {
                        if let Ok(leader_meta) = fs::symlink_metadata(leader_path) {
                            #[cfg(unix)]
                            {
                                use std::os::unix::fs::MetadataExt;
                                if link_meta.dev() == leader_meta.dev()
                                    && link_meta.ino() == leader_meta.ino()
                                {
                                    continue;
                                }
                            }
                            #[cfg(not(unix))]
                            {
                                let _ = (link_meta, leader_meta);
                            }
                        }
                        let _ = fs::remove_file(&dest_path);
                    }

                    if let Some(parent) = dest_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }

                    if let Err(e) = fs::hard_link(leader_path, &dest_path) {
                        debug_log!(
                            Recv,
                            1,
                            "failed to hard link {} => {}: {}",
                            dest_path.display(),
                            leader_path.display(),
                            e
                        );
                    }
                }
            }
        }
    }

    /// Creates a single directory during incremental processing.
    ///
    /// On success, returns `Ok(true)`. On failure or skip, marks the directory
    /// as failed and returns `Ok(false)`. Only returns `Err` for unrecoverable errors.
    pub(super) fn create_directory_incremental(
        &self,
        dest_dir: &std::path::Path,
        entry: &FileEntry,
        metadata_opts: &MetadataOptions,
        failed_dirs: &mut FailedDirectories,
        acl_cache: Option<&AclCache>,
    ) -> io::Result<bool> {
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
            return Ok(false);
        }

        // Try to create the directory
        if !dir_path.exists() {
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
                return Ok(false);
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

        Ok(true)
    }
}
