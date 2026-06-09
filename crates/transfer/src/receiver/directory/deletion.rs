//! Extraneous file deletion at the destination.
//!
//! Implements `--delete` scanning: groups file list entries by parent directory,
//! scans destination directories for entries absent from the source list, and
//! removes them. Parallel scanning via `map_blocking` when directory count
//! exceeds threshold. Respects `--max-delete` via an atomic counter shared
//! across workers, and `FilterChain::allows_deletion()` for protect/risk rules.
//!
//! SEC-1.q2: when the receiver carries a [`fast_io::DirSandbox`], the scan
//! and removal syscalls route through the `*_via_sandbox_or_fallback`
//! helpers so a TOCTOU symlink swap on a top-level entry cannot redirect
//! the listing or the unlink to an attacker-chosen inode. Multi-component
//! relative paths take the documented path-based fallback (see
//! `crates/fast_io/src/dir_sandbox/at_syscalls.rs::single_component_leaf`).

use std::io;
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};
use protocol::stats::DeleteStats;

use super::normalize_filename_for_compare;
use crate::receiver::ReceiverContext;

impl ReceiverContext {
    /// Deletes extraneous files at the destination that are not in the received file list.
    ///
    /// Groups file list entries by parent directory, then for each destination directory,
    /// scans for entries not present in the source list and removes them. Directories
    /// are removed recursively (depth-first).
    ///
    /// Uses tokio `spawn_blocking` + semaphore for parallel directory scanning when
    /// directory count exceeds threshold. When `max_delete` is set, an atomic counter
    /// enforces the deletion limit across all parallel workers. Protect/risk filter
    /// rules are evaluated via `FilterChain::allows_deletion()` before each deletion.
    ///
    /// `sandbox` is the SEC-1.e parent-dirfd carrier opened at setup time. When
    /// `Some`, the scan and per-entry deletions route through the sandbox-anchored
    /// `*_via_sandbox_or_fallback` helpers (audit rows #5, #6, #7); when `None`
    /// every site falls back to the path-based `std::fs` syscalls and is
    /// byte-identical to the pre-SEC-1.q2 behaviour.
    ///
    /// Returns `(stats, limit_exceeded)` where `limit_exceeded` is true when deletions
    /// were stopped due to `--max-delete`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:delete_in_dir()` - scans one directory, removes unlisted entries
    /// - `generator.c:do_delete_pass()` - full tree walk deletion sweep
    /// - `main.c:1367` - `deletion_count >= max_delete` check
    /// - `exclude.c:check_filter()` - is_excluded() before deletion
    pub(in crate::receiver) fn delete_extraneous_files<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
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

        // Share directory children map and filter chain across workers.
        // The filter chain clone captures the global rules snapshot for
        // allows_deletion() checks. Per-directory merge files are not
        // re-read during deletion (upstream also only evaluates the
        // pre-loaded filter list in delete_in_dir).
        let dir_children = Arc::new(dir_children);
        let filter_chain = Arc::new(self.filter_chain.clone());
        let dest_dir_owned = dest_dir.to_path_buf();
        // SEC-1.q2: clone the sandbox `Arc` into the worker closure so
        // every per-directory job can route its scan and per-entry
        // deletions through the sandbox-anchored `*at` helpers without
        // contending on a mutex. The carrier is `None` when the
        // destination dir could not be opened at setup time, and on
        // Windows where the carrier is not used (NTFS handle semantics
        // already close the symlink-swap window, per the SEC-1.l audit).
        #[cfg(unix)]
        let sandbox_for_workers = sandbox.cloned();

        // Collect deleted relative paths for post-parallel itemize emission.
        // The writer is not Send, so MSG_INFO frames are emitted sequentially
        // after parallel deletion completes.
        let per_dir_results: Vec<(DeleteStats, Vec<PathBuf>)> = crate::parallel_io::map_blocking(
            dirs_to_scan,
            self.parallel_thresholds
                .for_op(crate::parallel_io::ParallelOp::Deletion),
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

                #[cfg(unix)]
                let sandbox_ref = sandbox_for_workers.as_deref();
                #[cfg(unix)]
                let read_dir_iter = {
                    // SEC-1.q2 audit row #5: anchor the directory listing
                    // on the sandbox dirfd when the scan target is the
                    // root or a single-component subdir; the helper falls
                    // back to `std::fs::read_dir` for multi-component
                    // descents and the sandbox-off case.
                    let scan_rel: &Path = if dir_relative.as_os_str() == "." {
                        Path::new("")
                    } else {
                        dir_relative.as_path()
                    };
                    match fast_io::read_dir_via_sandbox_or_fallback(
                        sandbox_ref,
                        &dest_dir_owned,
                        scan_rel,
                        &dest_path,
                    ) {
                        Ok(iter) => iter,
                        Err(_) => return (DeleteStats::new(), Vec::new()),
                    }
                };
                #[cfg(not(unix))]
                let read_dir_iter = match std::fs::read_dir(&dest_path) {
                    Ok(iter) => iter,
                    Err(_) => return (DeleteStats::new(), Vec::new()),
                };

                let mut stats = DeleteStats::new();
                let mut deleted_paths = Vec::new();
                for entry in read_dir_iter {
                    #[cfg(unix)]
                    let (name, kind) = match entry {
                        Ok(view) => (view.file_name().to_os_string(), view.file_type()),
                        Err(_) => continue,
                    };
                    #[cfg(unix)]
                    let is_dir = kind.is_some_and(fast_io::EntryKind::is_dir);
                    #[cfg(unix)]
                    let is_symlink = kind.is_some_and(fast_io::EntryKind::is_symlink);

                    #[cfg(not(unix))]
                    let entry = match entry {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    #[cfg(not(unix))]
                    let name = entry.file_name();
                    #[cfg(not(unix))]
                    let file_type = entry.file_type().ok();
                    #[cfg(not(unix))]
                    let is_dir = file_type.as_ref().is_some_and(|ft| ft.is_dir());
                    #[cfg(not(unix))]
                    let is_symlink = file_type.as_ref().is_some_and(|ft| ft.is_symlink());

                    let normalized = normalize_filename_for_compare(&name);
                    if keep.contains(&normalized) {
                        continue;
                    }

                    // upstream: generator.c:delete_in_dir() - is_excluded()
                    // check before deletion. allows_deletion() evaluates
                    // protect/risk rules from the global filter chain.
                    //
                    // Strip the implicit "." directory prefix when scanning
                    // the deletion root so a glob like `?` does not see the
                    // dot as a single-character directory component. Without
                    // this, descendant matchers (e.g. `?/**`) would match
                    // top-level deletion candidates as if they sat under a
                    // single-char parent, suppressing legitimate deletes.
                    let rel_for_filter = if dir_relative.as_os_str() == "." {
                        PathBuf::from(&name)
                    } else {
                        dir_relative.join(&name)
                    };
                    if !filter_chain.allows_deletion(&rel_for_filter, is_dir) {
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
                    // SEC-1.q2: the unlink/rmdir-all also routes through
                    // the sandbox helpers so the deletion is anchored on
                    // the same parent the scan observed. The relative
                    // path passed here is rooted at the receiver's
                    // sandbox base (`dest_dir_owned`); top-level
                    // entries take the sandbox-anchored fast path and
                    // deeper entries fall back to the path-based
                    // `std::fs::remove_*` helpers via the same
                    // `single_component_leaf` precondition the existing
                    // SEC-1.f-j helpers use.
                    #[cfg(unix)]
                    let rel_for_unlink = if dir_relative.as_os_str() == "." {
                        std::path::PathBuf::from(&name)
                    } else {
                        dir_relative.join(&name)
                    };

                    let result = if is_dir {
                        // SEC-1.q2 audit row #6
                        #[cfg(unix)]
                        {
                            fast_io::recursive_unlinkat_via_sandbox_or_fallback(
                                sandbox_ref,
                                &dest_dir_owned,
                                &rel_for_unlink,
                                &path,
                            )
                        }
                        #[cfg(not(unix))]
                        {
                            std::fs::remove_dir_all(&path)
                        }
                    } else {
                        // SEC-1.q2 audit row #7
                        #[cfg(unix)]
                        {
                            fast_io::unlink_via_sandbox_or_fallback(
                                sandbox_ref,
                                &dest_dir_owned,
                                &rel_for_unlink,
                                &path,
                                fast_io::UnlinkFlags::File,
                            )
                        }
                        #[cfg(not(unix))]
                        {
                            std::fs::remove_file(&path)
                        }
                    };

                    match result {
                        Ok(()) => {
                            // Compute relative path for itemize output.
                            // upstream: delete.c:180 - log_delete(fbuf) emits the
                            // filename relative to the deletion root. Top-level
                            // entries are emitted as bare names ("delete.txt"),
                            // not "./delete.txt", matching upstream output.
                            let rel = if dir_relative.as_os_str() == "." {
                                PathBuf::from(&name)
                            } else {
                                dir_relative.join(&name)
                            };
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
