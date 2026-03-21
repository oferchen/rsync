//! Extraneous file deletion at the destination.
//!
//! Implements `--delete` scanning: groups file list entries by parent directory,
//! scans destination directories for entries absent from the source list, and
//! removes them. Parallel scanning via `map_blocking` when directory count
//! exceeds threshold. Respects `--max-delete` via an atomic counter shared
//! across workers.

use std::fs;
use std::io;
use std::path::Path;

use logging::{debug_log, info_log};
use protocol::stats::DeleteStats;

use super::normalize_filename_for_compare;
use crate::receiver::{PARALLEL_STAT_THRESHOLD, ReceiverContext};

impl ReceiverContext {
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
    pub(in crate::receiver) fn delete_extraneous_files<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        dest_dir: &Path,
        writer: &mut W,
    ) -> io::Result<(DeleteStats, bool)> {
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let max_delete = self.config.deletion.max_delete;

        // Build directory -> children map from the file list.
        // Use owned OsString keys so the map can be shared across threads.
        // On macOS, normalize filenames to NFC so that NFD names from read_dir
        // match NFC names from the sender's file list.
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
                    .insert(normalize_filename_for_compare(name));
            }
        }

        let dirs_to_scan: Vec<PathBuf> = dir_children.keys().cloned().collect();

        // Atomic counter for max_delete enforcement across parallel workers.
        // upstream: main.c:1367 - deletion_count >= max_delete
        let deletions_performed = Arc::new(AtomicU64::new(0));

        // Share directory children map across workers.
        let dir_children = Arc::new(dir_children);
        let dest_dir_owned = dest_dir.to_path_buf();

        // Collect deleted relative paths for post-parallel itemize emission.
        // The writer is not Send, so MSG_INFO frames are emitted sequentially
        // after parallel deletion completes.
        let per_dir_results: Vec<(DeleteStats, Vec<PathBuf>)> = crate::parallel_io::map_blocking(
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
                    None => return (DeleteStats::new(), Vec::new()),
                };

                let read_dir = match fs::read_dir(&dest_path) {
                    Ok(iter) => iter,
                    Err(_) => return (DeleteStats::new(), Vec::new()),
                };

                let mut stats = DeleteStats::new();
                let mut deleted_paths = Vec::new();
                for entry in read_dir {
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    let name = entry.file_name();
                    let normalized = normalize_filename_for_compare(&name);
                    if keep.contains(&normalized) {
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
                            // Compute relative path for itemize output
                            let rel = dir_relative.join(&name);
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
                            deleted_paths.push(rel);
                        }
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => {
                            debug_log!(Del, 1, "failed to delete {}: {}", path.display(), e);
                        }
                    }
                }
                (stats, deleted_paths)
            },
        );

        let mut combined = DeleteStats::new();
        for (s, deleted_paths) in &per_dir_results {
            combined.files = combined.files.saturating_add(s.files);
            combined.dirs = combined.dirs.saturating_add(s.dirs);
            combined.symlinks = combined.symlinks.saturating_add(s.symlinks);
            combined.devices = combined.devices.saturating_add(s.devices);
            combined.specials = combined.specials.saturating_add(s.specials);

            // upstream: log.c:log_delete() - emit "*deleting" itemize for each deleted item
            if self.should_emit_itemize() {
                for rel_path in deleted_paths {
                    let line = format!("*deleting   {}\n", rel_path.display());
                    let _ = writer.send_msg_info(line.as_bytes());
                }
            }
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
}
