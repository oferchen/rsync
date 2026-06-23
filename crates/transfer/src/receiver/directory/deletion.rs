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
use std::path::Path;

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
    /// Uses `crate::parallel_io::map_blocking` (rayon's work-stealing pool) for
    /// parallel directory scanning when the directory count exceeds the
    /// `ParallelOp::Deletion` threshold. When `max_delete` is set, an atomic counter
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
    ) -> io::Result<(DeleteStats, bool, i32)> {
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        let max_delete = self.config.deletion.max_delete;

        // Whether the deletion chain carries per-directory merge configs
        // (`.rsync-filter`, dir-merge `.filt`/`.filt2`). Computed up front so it
        // can gate both the scan-target expansion below and the per-worker
        // reload further down. When there are no per-dir merge configs the
        // deletion pass behaves exactly as before: dirs_to_scan is keyed off
        // parents-with-a-visible-child and the flat global chain decides.
        let needs_perdir_merge = self.deletion_filter_chain.has_per_dir_merge();

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

            // upstream: generator.c:delete_in_dir() runs for EVERY content
            // directory in the file list. Each directory's deletion candidate
            // list is built from a fresh readdir of the destination, regardless
            // of whether any of that directory's source children are visible in
            // the flist. A directory whose only source children are filter-
            // hidden (e.g. `--filter='hide,! */'`) must still be scanned so
            // extraneous destination entries inside it are removed. Keying
            // dirs_to_scan solely off parents-with-a-visible-child skips such
            // directories and leaves extraneous files undeleted, so register
            // every directory entry as its own scan target with a (possibly
            // empty) keep-set. Gated on `needs_perdir_merge`: the expanded
            // scan set is only safe when the per-directory merge rules are
            // reloaded per dir to protect dest-side merge files and their
            // excludes; without them, scanning extra dirs against the flat
            // chain would over-delete. Daemon/non-merge transfers keep the
            // master scan set unchanged.
            if needs_perdir_merge && entry.is_dir() {
                dir_children.entry(relative.to_path_buf()).or_default();
            }
        }

        let dirs_to_scan: Vec<PathBuf> = dir_children.keys().cloned().collect();

        // Atomic counter for max_delete enforcement across parallel workers.
        // upstream: main.c:1367 - deletion_count >= max_delete
        let deletions_performed = Arc::new(AtomicU64::new(0));

        // Share directory children map and filter chains across workers.
        //
        // Two chains are carried: `flat_chain` is the global rules snapshot
        // used when no per-directory merge files are in play, and `merge_chain`
        // is the deletion-pass chain that knows about per-directory merge
        // configs (`.rsync-filter`, dir-merge `.filt`/`.filt2`).
        //
        // upstream: generator.c:308 delete_in_dir() ->
        // change_local_filter_dir() -> exclude.c:push_local_filters() reloads
        // each destination directory's per-directory merge file(s) (including
        // nested merges) before is_excluded() tests a deletion candidate, so a
        // merge file's own protect rule (and any merge-driven excludes) take
        // effect during deletion. Carry the transfer root so leading-`/` rules
        // in those merge files re-anchor correctly, and remember whether any
        // per-directory merge configs exist so workers only pay the reload cost
        // when the chain actually has per-dir merges.
        let dir_children = Arc::new(dir_children);
        let flat_chain = Arc::new(self.filter_chain.clone());
        let merge_chain = Arc::new({
            let mut chain = self.deletion_filter_chain.clone();
            chain.set_transfer_root(dest_dir.to_path_buf());
            chain
        });
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
        //
        // EDG-SANDBOX.A: each worker also threads back an `Option<io::Error>`
        // so a sandbox-class failure on `read_dir` is propagated to the
        // outer caller instead of being silently coerced to empty stats.
        // EACCES is the upstream-parity non-fatal class (matches
        // `generator.c:delete_in_dir` where a permission failure leaves the
        // directory alone and the io_error bit drives a non-zero exit);
        // every other class (ELOOP from a chdir-symlink swap,
        // EOPNOTSUPP/Unsupported from a sandbox-anchored refusal,
        // ENOTDIR from a planted file on the scan target) is fail-loud.
        let per_dir_results: Vec<(DeleteStats, Vec<PathBuf>, Option<io::Error>)> =
            crate::parallel_io::map_blocking(
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
                        None => return (DeleteStats::new(), Vec::new(), None),
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
                            Err(e) => return classify_scan_error(e),
                        }
                    };
                    #[cfg(not(unix))]
                    let read_dir_iter = match std::fs::read_dir(&dest_path) {
                        Ok(iter) => iter,
                        Err(e) => return classify_scan_error(e),
                    };

                    let mut stats = DeleteStats::new();
                    let mut deleted_paths = Vec::new();

                    // upstream parity: reload this destination directory's
                    // per-directory merge rules (and its inheriting ancestors')
                    // so dir-merge self-exclusion and merge-driven excludes are
                    // active while deciding deletions. Only entered when the
                    // deletion chain has per-dir merge configs; otherwise the
                    // flat global chain is consulted directly. enter_directory
                    // takes `&mut self`, so each worker reloads onto its own
                    // clone of the merge chain.
                    let local_chain = if needs_perdir_merge {
                        let mut chain = (*merge_chain).clone();
                        let _ = chain.enter_directory(&dest_dir_owned);
                        if dir_relative.as_os_str() != "." {
                            let mut cur = dest_dir_owned.clone();
                            for comp in dir_relative.iter() {
                                cur.push(comp);
                                let _ = chain.enter_directory(&cur);
                            }
                        }
                        Some(chain)
                    } else {
                        None
                    };

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
                        let allows = match &local_chain {
                            Some(chain) => chain.allows_deletion(&rel_for_filter, is_dir),
                            None => flat_chain.allows_deletion(&rel_for_filter, is_dir),
                        };
                        if !allows {
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
                            Err(e) => {
                                debug_log!(Del, 1, "failed to delete {}: {}", path.display(), e);
                                // EDG-SANDBOX.B: same discriminator as the
                                // scan-error helper. EACCES / NotFound are
                                // upstream-parity non-fatal classes; every
                                // other class (ELOOP/EOPNOTSUPP/ENOTDIR/EPERM)
                                // is a security boundary the worker must
                                // surface so the outer caller's `Err`
                                // propagation produces a non-zero exit.
                                if let Some(err) = fail_loud_unlink_error(e) {
                                    return (stats, deleted_paths, Some(err));
                                }
                            }
                        }
                    }
                    (stats, deleted_paths, None)
                },
            );

        let mut combined = DeleteStats::new();
        // UTS-16.b: any worker that hit a fail-loud sandbox class (ELOOP /
        // EOPNOTSUPP / ENOTDIR / EPERM) surfaces IOERR_GENERAL here so the
        // receiver's overall io_error bit drives a non-zero exit (RERR_PARTIAL=23)
        // instead of either silently skipping or aborting the whole receiver
        // pass.
        let mut io_err_bits: i32 = 0;
        for (s, deleted_paths, worker_err) in &per_dir_results {
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

            if worker_err.is_some() {
                io_err_bits |= crate::generator::io_error_flags::IOERR_GENERAL;
            }
        }

        // Limit is exceeded when we had candidates beyond the allowed count.
        let total_deletions = u64::from(combined.files)
            + u64::from(combined.dirs)
            + u64::from(combined.symlinks)
            + u64::from(combined.devices)
            + u64::from(combined.specials);
        let limit_exceeded = max_delete.is_some_and(|limit| total_deletions >= limit);

        Ok((combined, limit_exceeded, io_err_bits))
    }
}

/// Classifies a `read_dir` failure inside the parallel deletion worker.
///
/// Returns the worker tuple with the error threaded into the third slot
/// only when the error is fail-loud: ELOOP from a chdir-symlink swap,
/// EOPNOTSUPP from a sandbox-anchored refusal, ENOTDIR from a planted
/// file on the scan target, and every other non-EACCES/NotFound class.
/// EACCES is the upstream-parity non-fatal class (matches
/// `generator.c:delete_in_dir` where a permission failure leaves the
/// directory alone and the io_error bit drives the non-zero exit).
/// NotFound mirrors upstream's continue-on-vanished semantics: a
/// directory that disappeared between the file-list snapshot and the
/// scan is benign and must not stop the rest of the sweep.
///
/// # Upstream Reference
///
/// - `generator.c:delete_in_dir()` - "delete_in_dir: opendir failed"
///   path classifies EACCES as non-fatal (io_error bit only) and every
///   other class as a fatal scan failure.
fn classify_scan_error(e: io::Error) -> (DeleteStats, Vec<std::path::PathBuf>, Option<io::Error>) {
    match fail_loud_unlink_error(e) {
        Some(err) => (DeleteStats::new(), Vec::new(), Some(err)),
        None => (DeleteStats::new(), Vec::new(), None),
    }
}

/// Classifies an unlink/scan failure as fail-loud or upstream-parity.
///
/// Returns `Some(e)` when the error class is a security boundary the
/// receiver must surface as a non-zero exit (ELOOP from a TOCTOU swap,
/// EOPNOTSUPP / `Unsupported` from a sandbox-anchored refusal, ENOTDIR
/// from a planted file on the scan target, EPERM from a chattr-immutable
/// target). Returns `None` for the upstream-parity non-fatal classes:
/// EACCES (matches `delete.c:144-176 delete_item` where a permission
/// failure increments the io_error bit and continues) and NotFound
/// (matches the continue-on-vanished semantics).
///
/// # Upstream Reference
///
/// - `delete.c:144-176 delete_item` - EACCES is non-fatal; every other
///   class is rsyserr+set_io_error()+continue, which drives a non-zero
///   `g_exit_code = RERR_PARTIAL` via the io_error bit.
fn fail_loud_unlink_error(e: io::Error) -> Option<io::Error> {
    if e.kind() == io::ErrorKind::PermissionDenied || e.kind() == io::ErrorKind::NotFound {
        None
    } else {
        Some(e)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// EDG-SANDBOX.A/B contract test: discrimination between the
    /// upstream-parity non-fatal classes (EACCES, NotFound) and the
    /// fail-loud security boundaries (everything else).
    ///
    /// The pre-fix `Err(_) => debug_log!` and `Err(_) => return empty
    /// stats` patterns dropped every class without distinction. The fix
    /// routes EACCES/NotFound through the upstream-parity branch and
    /// surfaces every other class as `Some(e)` so the outer collector
    /// produces a non-zero `io::Result`.
    #[test]
    fn fail_loud_unlink_error_discriminates_by_class() {
        // EACCES is the upstream-parity non-fatal branch.
        // upstream: delete.c:144-176 delete_item - permission denied is
        // non-fatal; the io_error bit drives the non-zero exit.
        let eacces = io::Error::from(io::ErrorKind::PermissionDenied);
        assert!(
            fail_loud_unlink_error(eacces).is_none(),
            "EACCES must take the upstream-parity non-fatal branch",
        );

        // NotFound matches upstream's continue-on-vanished semantics.
        let enoent = io::Error::from(io::ErrorKind::NotFound);
        assert!(
            fail_loud_unlink_error(enoent).is_none(),
            "NotFound must take the continue-on-vanished branch",
        );

        // ELOOP is the canonical fail-loud sandbox-class error: a
        // mid-syscall symlink swap on the leaf surfaces as ELOOP under
        // `openat2(RESOLVE_NO_SYMLINKS)` (Linux) and `openat(O_NOFOLLOW)`
        // on the path-based fallback.
        let eloop = io::Error::from_raw_os_error(libc::ELOOP);
        let propagated = fail_loud_unlink_error(eloop)
            .expect("ELOOP must surface as Err so the receiver exits non-zero");
        assert_ne!(propagated.kind(), io::ErrorKind::PermissionDenied);
        assert_ne!(propagated.kind(), io::ErrorKind::NotFound);

        // ENOTDIR is the macOS/BSD fail-loud class produced when the
        // sandbox finds a non-directory at the resolved path (a planted
        // file at the scan target).
        let enotdir = io::Error::from_raw_os_error(libc::ENOTDIR);
        assert!(
            fail_loud_unlink_error(enotdir).is_some(),
            "ENOTDIR must surface as Err (planted-file-where-dir trap)",
        );

        // EOPNOTSUPP / `Unsupported` is the sandbox-anchored refusal
        // class. The fix must propagate it instead of pretending the
        // unlink succeeded.
        let eopnotsupp = io::Error::from_raw_os_error(libc::EOPNOTSUPP);
        assert!(
            fail_loud_unlink_error(eopnotsupp).is_some(),
            "EOPNOTSUPP must surface as Err (sandbox-anchored refusal)",
        );
    }

    /// EDG-SANDBOX.A: the parallel worker's tuple-shape contract -
    /// `classify_scan_error` reuses the same discrimination so a `read_dir`
    /// failure routes through identical fail-loud / non-fatal logic as the
    /// unlink path.
    #[test]
    fn classify_scan_error_threads_fail_loud_class() {
        let eloop = io::Error::from_raw_os_error(libc::ELOOP);
        let (stats, paths, worker_err) = classify_scan_error(eloop);
        assert_eq!(stats.total(), 0);
        assert!(paths.is_empty());
        let err = worker_err
            .expect("ELOOP on read_dir must propagate as Err so the outer caller exits non-zero");
        assert_ne!(err.kind(), io::ErrorKind::PermissionDenied);

        // EACCES on the scan is the upstream-parity non-fatal branch -
        // matches `generator.c:delete_in_dir` "opendir failed" path.
        let eacces = io::Error::from(io::ErrorKind::PermissionDenied);
        let (_stats, _paths, worker_err) = classify_scan_error(eacces);
        assert!(
            worker_err.is_none(),
            "EACCES on scan must take the upstream-parity non-fatal branch",
        );
    }
}
