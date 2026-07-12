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
//! `crates/fast_io/src/dir_sandbox/at_syscalls/lstat.rs::single_component_leaf`).

use std::cmp::Ordering;
use std::io;
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};
use protocol::flist::{FileEntry, compare_file_entries};
use protocol::stats::DeleteStats;

use super::normalize_filename_for_compare;
use crate::receiver::ReceiverContext;

/// A single deleted destination entry carried out of the parallel scan
/// workers so its `deleting`/`*deleting` line can be emitted in upstream's
/// deterministic per-directory sorted order rather than the hash-random
/// order the `HashMap`-keyed scan set would otherwise produce.
struct DeletedEntry {
    /// Path relative to the deletion root, as printed by upstream
    /// `log_delete()` (top-level entries are bare names, not `./name`).
    rel: PathBuf,
    /// Whether the entry is a directory. Directories sort after files at a
    /// given level (upstream `t_PATH`) and print with a trailing slash.
    is_dir: bool,
}

/// Orders deleted entries to match upstream's observable delete stream.
///
/// Upstream's generator walks the file list ascending by `f_name_cmp` and
/// calls `generator.c:delete_in_dir()` once per directory in that order.
/// Each `delete_in_dir()` scans its directory's dirlist - sorted ascending
/// by `f_name_cmp` (files before dirs at a given level) - and iterates it in
/// reverse (`for (i = dirlist->used; i--; )`), so a directory's own
/// extraneous entries are emitted in descending order.
///
/// This deletion pass scans each file-list directory as its own worker and
/// removes immediate extraneous entries (doomed subdirectories are removed
/// whole via `recursive_unlinkat`, which emits only the subdirectory's own
/// line). The observable stream is therefore: directories (the parent of
/// each deleted entry) processed in ascending `f_name_cmp` order, and within
/// each directory the entries emitted in descending `f_name_cmp` order.
/// Deriving the order from the flat deleted set here makes the emitted
/// sequence deterministic and upstream-matching regardless of the parallel
/// scan/unlink order.
///
/// # Upstream Reference
///
/// - `generator.c:delete_in_dir()` - sorted dirlist, reverse iteration
/// - `generator.c:2328` generate_files loop - one delete_in_dir() per
///   directory, in ascending file-list order
/// - `flist.c:fsort()` / `f_name_cmp()` - the ascending comparator
fn order_deletions_upstream(entries: Vec<DeletedEntry>) -> Vec<DeletedEntry> {
    use std::collections::BTreeMap;

    // Group entries by parent directory (the directory that was scanned).
    // A BTreeMap keyed on the f_name_cmp order of the parent directory keeps
    // the groups in the ascending order upstream's generator visits them.
    let mut groups: BTreeMap<DirKey, Vec<DeletedEntry>> = BTreeMap::new();
    for entry in entries {
        let parent = entry
            .rel
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        groups.entry(DirKey(parent)).or_default().push(entry);
    }

    let mut ordered = Vec::new();
    for (_dir, mut group) in groups {
        // Ascending f_name_cmp, then reverse: descending within the
        // directory, matching upstream's reverse dirlist iteration.
        group.sort_by(|a, b| f_name_cmp_full(&a.rel, a.is_dir, &b.rel, b.is_dir));
        group.reverse();
        ordered.extend(group);
    }
    ordered
}

/// A scanned-directory key ordered by upstream `f_name_cmp` so the parent
/// directories are visited in the same ascending order the generator walks
/// the file list. Directories are always compared as directory entries.
struct DirKey(PathBuf);

impl PartialEq for DirKey {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for DirKey {}
impl PartialOrd for DirKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for DirKey {
    fn cmp(&self, other: &Self) -> Ordering {
        f_name_cmp_full(&self.0, true, &other.0, true)
    }
}

/// Ascending upstream `f_name_cmp` over two relative paths, treating each
/// as a dir or non-dir entry so the protocol-29 `t_PATH`/`t_ITEM`
/// files-before-dirs ordering upstream's dirlist sort relies on is
/// reproduced. `compare_file_entries` is the full comparator (as opposed to
/// the byte-only `f_name_cmp`), matching `flist.c:f_name_cmp` at
/// protocol >= 29.
fn f_name_cmp_full(a: &Path, a_is_dir: bool, b: &Path, b_is_dir: bool) -> Ordering {
    let ea = make_entry(a, a_is_dir);
    let eb = make_entry(b, b_is_dir);
    compare_file_entries(&ea, &eb)
}

fn make_entry(path: &Path, is_dir: bool) -> FileEntry {
    if is_dir {
        FileEntry::new_directory(path.to_path_buf(), 0o755)
    } else {
        FileEntry::new_file(path.to_path_buf(), 0, 0o644)
    }
}

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

        // The deletion decision must consult the chain that actually carries
        // the filter rules for this role. A server-side receiver reads the
        // client's rules off the wire into `filter_chain` (setup/context.rs
        // branch A). A client-side pull never receives the wire filter list,
        // so its rules live in the dedicated `deletion_filter_chain` built from
        // the local CLI rules (branch B) and `filter_chain` is empty there.
        // Pick whichever is populated so a plain `- name`, a receiver-side
        // `-r name`, or a perishable `-p name` rule protects a matching
        // destination entry from --delete on both sides, mirroring upstream
        // generator.c:delete_in_dir() which filters the get_dirlist()
        // candidates through the same rule list regardless of transfer role.
        let deletion_chain = if self.deletion_filter_chain.is_empty() {
            &self.filter_chain
        } else {
            &self.deletion_filter_chain
        };

        // Whether the deletion chain carries per-directory merge configs
        // (`.rsync-filter`, dir-merge `.filt`/`.filt2`). Computed up front so it
        // can gate both the scan-target expansion below and the per-worker
        // reload further down. When there are no per-dir merge configs the
        // deletion pass behaves exactly as before: dirs_to_scan is keyed off
        // parents-with-a-visible-child and the flat global chain decides.
        let needs_perdir_merge = deletion_chain.has_per_dir_merge();

        // Build directory -> children map from the file list.
        // Use owned OsString keys so the map can be shared across threads.
        // On macOS, normalize filenames to NFC so that NFD names from read_dir
        // match NFC names from the sender's file list.
        let mut dir_children: HashMap<PathBuf, HashSet<std::ffi::OsString>> = HashMap::new();

        // upstream: generator.c:do_delete_pass() (376), recv_generator (1534),
        // and the inc-recurse delete-during loop (2317) all call delete_in_dir()
        // for a flist directory ONLY when it carries FLAG_CONTENT_DIR - a
        // directory the sender actually recursed into. A non-content dir gets
        // change_local_filter_dir() instead and is never scanned for deletion.
        // The transfer root "." is a content dir for a recursive transfer but
        // not for --files-from, where the root is sent as an implied dir
        // (flist.c:2419 send_file_name(".", ... & ~FLAG_CONTENT_DIR), decoded as
        // content_dir() == false). Likewise every implied parent dir created
        // under --files-from / --relative clears FLAG_CONTENT_DIR
        // (flist.c:1949). Only content dirs are scan targets; scanning an
        // implied dir would delete a stale destination file inside it that
        // upstream preserves (DATA-LOSS).
        //
        // `content_dirs` records exactly which file-list directories carry the
        // flag, so `dirs_to_scan` can be filtered to them below. The keep-set
        // map (`dir_children`) is still built for every parent - it only decides
        // which children survive a scan, not whether the parent is scanned.
        let mut root_is_content_dir = false;
        let mut content_dirs: HashSet<PathBuf> = HashSet::new();

        for entry in &self.file_list {
            let relative = entry.path();
            if relative.as_os_str() == "." {
                if entry.is_dir() && entry.content_dir() {
                    root_is_content_dir = true;
                }
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
            // directory in the file list, regardless of whether any of that
            // directory's source children are visible in the flist. A content
            // directory whose source children are all filter-hidden (e.g.
            // `--filter='hide,! */'`) must still be scanned so its extraneous
            // destination entries are removed; keying the scan set solely off
            // parents-with-a-visible-child leaves those entries undeleted.
            // Register every content directory as its own scan target with a
            // (possibly empty) keep-set. An implied (non-content) directory is
            // NOT a scan target - upstream skips its delete_in_dir() entirely -
            // so it is deliberately excluded here and filtered out of
            // `dirs_to_scan` below. Protection of entries that must survive a
            // scanned dir is the responsibility of the per-candidate
            // `allows_deletion` check below - which consults the per-directory
            // merge chain when one is reloaded for the directory, and otherwise
            // the flat global chain (complete when no per-dir merges exist) -
            // not of pruning the scan set.
            if entry.is_dir() && entry.content_dir() {
                dir_children.entry(relative.to_path_buf()).or_default();
                content_dirs.insert(relative.to_path_buf());
            }
        }

        // upstream: generator.c:do_delete_pass() runs delete_in_dir(".") only
        // when the received root carries FLAG_CONTENT_DIR. For a recursive
        // transfer the root is a content dir even when every source entry is
        // filter-excluded (e.g. `--exclude='*' --delete-excluded`, whose file
        // list contains only "."), so top-level extraneous entries are still
        // removed. For --files-from the root is an implied (non-content) dir, so
        // its stale top-level destination entries are preserved. Register "."
        // with a (possibly empty) keep-set only in the content-dir case.
        if root_is_content_dir {
            dir_children.entry(PathBuf::from(".")).or_default();
            content_dirs.insert(PathBuf::from("."));
        }

        // Sort the scan set so directory processing is deterministic across
        // process runs. `HashMap::keys()` yields hash-randomized order, which
        // would make the emitted `deleting`/`*deleting` stream vary run to run.
        // The final emission order is re-derived from the deleted set in
        // `order_deletions_upstream`; sorting here keeps the scan/unlink work
        // itself reproducible without serializing it.
        // Restrict the scan set to content directories. The keep-set map keys
        // also include implied parent directories inferred from a visible
        // child (e.g. `subdir` for a `--files-from` entry `subdir/file`), which
        // upstream never scans for deletion (FLAG_CONTENT_DIR is clear). Scanning
        // one would delete a stale destination file inside the implied dir that
        // upstream preserves (DATA-LOSS). Filtering by `content_dirs` keeps the
        // scan targets exactly the directories whose received entry carries
        // FLAG_CONTENT_DIR, matching the generator.c:376/1534/2317 gate.
        let mut dirs_to_scan: Vec<PathBuf> = dir_children
            .keys()
            .filter(|dir| content_dirs.contains(*dir))
            .cloned()
            .collect();
        dirs_to_scan.sort_unstable();
        logging::debug_log!(
            Del,
            2,
            "delete pass scanning {} directories (needs_perdir_merge={needs_perdir_merge})",
            dirs_to_scan.len()
        );

        // --max-delete must count every filesystem entry actually removed,
        // including the leaves inside an extraneous directory, and stop
        // mid-traversal once the limit is reached. The parallel path below
        // removes a doomed subdirectory wholesale (`recursive_unlinkat`) and
        // counts it as a single deletion, silently exceeding the cap for
        // directory subtrees. Route capped runs through a serial,
        // leaf-granular executor that mirrors upstream delete.c:156/181
        // (guard-before-delete, increment-on-success).
        if let Some(limit) = max_delete {
            return self.delete_extraneous_files_capped(
                dest_dir,
                &dir_children,
                &dirs_to_scan,
                deletion_chain,
                needs_perdir_merge,
                #[cfg(unix)]
                sandbox,
                writer,
                limit,
            );
        }

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
        let flat_chain = Arc::new(deletion_chain.clone());
        let merge_chain = Arc::new({
            let mut chain = deletion_chain.clone();
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
        let per_dir_results: Vec<(DeleteStats, Vec<DeletedEntry>, Option<io::Error>)> =
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
                        None => return (DeleteStats::new(), Vec::<DeletedEntry>::new(), None),
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

                    // upstream: generator.c:delete_in_dir() emits this at
                    // `DEBUG_GTE(DEL, 2)` for every destination directory whose
                    // contents are scanned for deletion.
                    debug_log!(Del, 2, "delete_in_dir({})", dest_path.display());

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
                            // upstream: generator.c:delete_in_dir() - an excluded
                            // entry never enters get_dirlist()'s candidate set, so
                            // it is silently protected from deletion. We surface
                            // that protection at `DEBUG_GTE(DEL, 3)` so the
                            // per-candidate decision is observable, mirroring the
                            // `--debug=DEL` diagnostic granularity of upstream.
                            debug_log!(
                                Del,
                                3,
                                "not deleting {} (protected by filter rule)",
                                rel_for_filter.display()
                            );
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

                        // upstream: delete.c:delete_item() emits this at
                        // `DEBUG_GTE(DEL, 2)` just before removing the entry. The
                        // mode here carries only the file-type bits available from
                        // `read_dir` (perms are not needed to identify the item).
                        let type_bits = if is_dir {
                            0o040000
                        } else if is_symlink {
                            0o120000
                        } else {
                            0o100000
                        };
                        debug_log!(
                            Del,
                            2,
                            "delete_item({}) mode={:o}",
                            path.display(),
                            type_bits
                        );

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
                                    stats.dirs += 1;
                                } else if is_symlink {
                                    stats.symlinks += 1;
                                } else {
                                    stats.files += 1;
                                }
                                // The `deleting`/`*deleting` lines are emitted
                                // after the parallel pass in the deterministic
                                // upstream sorted order (see
                                // `order_deletions_upstream`); workers only
                                // record what was deleted so the unlinks stay
                                // parallel.
                                deleted_paths.push(DeletedEntry { rel, is_dir });
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
        let mut all_deleted: Vec<DeletedEntry> = Vec::new();
        // (populated below; reordered post-loop by order_deletions_upstream)
        for (s, deleted_paths, worker_err) in per_dir_results {
            combined.files = combined.files.saturating_add(s.files);
            combined.dirs = combined.dirs.saturating_add(s.dirs);
            combined.symlinks = combined.symlinks.saturating_add(s.symlinks);
            combined.devices = combined.devices.saturating_add(s.devices);
            combined.specials = combined.specials.saturating_add(s.specials);

            all_deleted.extend(deleted_paths);

            if worker_err.is_some() {
                io_err_bits |= crate::generator::io_error_flags::IOERR_GENERAL;
            }
        }

        // Emit the `deleting`/`*deleting` lines in upstream's deterministic
        // per-directory sorted order. The parallel workers unlinked (and
        // recorded) entries in hash-random / read_dir order; re-deriving the
        // emission order here keeps the observable output byte-for-byte
        // identical to upstream without serializing the unlinks.
        // upstream: log.c:log_delete() emits one line per deleted item.
        let all_deleted = order_deletions_upstream(all_deleted);
        let emit_itemize = self.should_emit_itemize();
        for entry in &all_deleted {
            // upstream: log.c:845 log_delete uses one "deleting %n" form; %n
            // (log.c:633-641) appends a trailing slash for directories, so a
            // dir prints "deleting sub/" - no word "directory".
            if entry.is_dir {
                info_log!(Del, 1, "deleting {}/", entry.rel.display());
            } else {
                info_log!(Del, 1, "deleting {}", entry.rel.display());
            }
            // upstream: log.c:log_delete() emits the "*deleting" itemize line
            // for each deleted item when --itemize-changes is active.
            if emit_itemize {
                let line = format!("*deleting   {}\n", entry.rel.display());
                let _ = writer.send_msg_info(line.as_bytes());
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

    /// Serial, leaf-granular deletion path used when `--max-delete` is set.
    ///
    /// The parallel path counts a doomed subdirectory as a single deletion and
    /// removes its subtree wholesale, so a directory holding N files costs one
    /// unit against the cap even though N+1 filesystem entries vanish. That
    /// undercount lets a small `--max-delete` value silently remove an
    /// unbounded number of files. This path walks every candidate depth-first
    /// in upstream reverse-sorted order and checks the cap before each
    /// individual removal, counting only successful deletions, mirroring
    /// upstream `delete.c:delete_item`/`delete_dir_contents`
    /// (`delete.c:156` guard, `delete.c:181` increment). Directory processing
    /// order is the same ascending `dirs_to_scan` order used elsewhere; the
    /// per-entry unlink and scan still route through the SEC-1.q2 sandbox
    /// helpers so the security posture is unchanged.
    #[allow(clippy::too_many_arguments)]
    fn delete_extraneous_files_capped<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        dir_children: &std::collections::HashMap<
            PathBuf,
            std::collections::HashSet<std::ffi::OsString>,
        >,
        dirs_to_scan: &[PathBuf],
        deletion_chain: &filters::FilterChain,
        needs_perdir_merge: bool,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
        limit: u64,
    ) -> io::Result<(DeleteStats, bool, i32)> {
        let mut state = CappedDeleteState {
            #[cfg(unix)]
            dest_dir,
            #[cfg(unix)]
            sandbox,
            limit,
            deleted: 0,
            skipped: 0,
            combined: DeleteStats::new(),
            io_err_bits: 0,
            emit_itemize: self.should_emit_itemize(),
            writer,
        };

        for dir_relative in dirs_to_scan {
            // Stop scanning once the cap is exhausted: every further candidate
            // would only add to the skipped count, and upstream stops issuing
            // deletions the moment the limit is reached.
            let dest_path = if dir_relative.as_os_str() == "." {
                dest_dir.to_path_buf()
            } else {
                dest_dir.join(dir_relative)
            };
            let Some(keep) = dir_children.get(dir_relative) else {
                continue;
            };

            // Reload this destination directory's per-directory merge rules so
            // dir-merge self-exclusion and merge-driven excludes stay active,
            // mirroring the parallel worker.
            let local_chain = if needs_perdir_merge {
                let mut chain = deletion_chain.clone();
                chain.set_transfer_root(dest_dir.to_path_buf());
                let _ = chain.enter_directory(dest_dir);
                if dir_relative.as_os_str() != "." {
                    let mut cur = dest_dir.to_path_buf();
                    for comp in dir_relative.iter() {
                        cur.push(comp);
                        let _ = chain.enter_directory(&cur);
                    }
                }
                Some(chain)
            } else {
                None
            };

            let scan_rel: &Path = if dir_relative.as_os_str() == "." {
                Path::new("")
            } else {
                dir_relative.as_path()
            };
            let entries = match state.scan_dir(scan_rel, &dest_path) {
                Ok(entries) => entries,
                Err(e) => {
                    if let Some(err) = fail_loud_unlink_error(e) {
                        return Err(err);
                    }
                    // EACCES / NotFound: upstream leaves the directory alone.
                    state.io_err_bits |= crate::generator::io_error_flags::IOERR_GENERAL;
                    continue;
                }
            };

            // Collect the extraneous candidates that survive the keep-set and
            // filter rules, then visit them in upstream reverse-sorted order.
            let mut candidates: Vec<CappedCandidate> = Vec::new();
            for (name, is_dir, is_symlink) in entries {
                let normalized = normalize_filename_for_compare(&name);
                if keep.contains(&normalized) {
                    continue;
                }
                let rel_for_filter = if dir_relative.as_os_str() == "." {
                    PathBuf::from(&name)
                } else {
                    dir_relative.join(&name)
                };
                let allows = match &local_chain {
                    Some(chain) => chain.allows_deletion(&rel_for_filter, is_dir),
                    None => deletion_chain.allows_deletion(&rel_for_filter, is_dir),
                };
                if !allows {
                    debug_log!(
                        Del,
                        3,
                        "not deleting {} (protected by filter rule)",
                        rel_for_filter.display()
                    );
                    continue;
                }
                candidates.push(CappedCandidate {
                    name,
                    rel: rel_for_filter,
                    is_dir,
                    is_symlink,
                });
            }
            // upstream: delete_in_dir iterates the sorted dirlist in reverse.
            // Sort ascending with the full comparator (files before dirs at a
            // level) then reverse so the prefix deleted when the cap trips
            // matches upstream's traversal.
            candidates.sort_by(|a, b| f_name_cmp_full(&a.rel, a.is_dir, &b.rel, b.is_dir));
            candidates.reverse();

            for candidate in candidates {
                let path = dest_path.join(&candidate.name);
                state.remove_entry(
                    &candidate.rel,
                    &path,
                    candidate.is_dir,
                    candidate.is_symlink,
                )?;
            }
        }

        let CappedDeleteState {
            combined,
            skipped,
            mut io_err_bits,
            ..
        } = state;
        if skipped > 0 {
            // upstream: generator.c:2430-2434 - one warning after the pass, then
            // `io_error |= IOERR_DEL_LIMIT` so the run exits RERR_DEL_LIMIT (25).
            // Nonreg renders at the default verbosity (info_verbosity[0]), the
            // same channel the sibling delete notices use.
            info_log!(
                Nonreg,
                1,
                "Deletions stopped due to --max-delete limit ({skipped} skipped)"
            );
            io_err_bits |= crate::generator::io_error_flags::IOERR_DEL_LIMIT;
        }
        Ok((combined, skipped > 0, io_err_bits))
    }
}

/// One extraneous destination entry awaiting a capped deletion decision.
struct CappedCandidate {
    name: std::ffi::OsString,
    rel: PathBuf,
    is_dir: bool,
    is_symlink: bool,
}

/// Mutable bookkeeping threaded through the recursive capped deletion walk.
struct CappedDeleteState<'w, W: ?Sized> {
    // Only read by the Unix sandbox-anchored scan path; the non-Unix scan
    // walks `target_path` directly, so gate the field like `sandbox` below.
    #[cfg(unix)]
    dest_dir: &'w Path,
    #[cfg(unix)]
    sandbox: Option<&'w std::sync::Arc<fast_io::DirSandbox>>,
    limit: u64,
    /// Successful deletions so far - the global cap counter
    /// (upstream `stats.deleted_files`).
    deleted: u64,
    /// Entries skipped because the cap was reached
    /// (upstream `skipped_deletes`).
    skipped: u64,
    combined: DeleteStats,
    io_err_bits: i32,
    emit_itemize: bool,
    writer: &'w mut W,
}

impl<W: crate::writer::MsgInfoSender + ?Sized> CappedDeleteState<'_, W> {
    /// Lists the immediate children of `target_path` as
    /// `(name, is_dir, is_symlink)`, routing through the sandbox helper on
    /// Unix so the listing is anchored the same way as the parallel worker.
    fn scan_dir(
        &self,
        relative: &Path,
        target_path: &Path,
    ) -> io::Result<Vec<(std::ffi::OsString, bool, bool)>> {
        #[cfg(unix)]
        {
            let mut out = Vec::new();
            let iter = fast_io::read_dir_via_sandbox_or_fallback(
                self.sandbox.map(|a| &**a),
                self.dest_dir,
                relative,
                target_path,
            )?;
            for entry in iter {
                let view = match entry {
                    Ok(view) => view,
                    Err(_) => continue,
                };
                let kind = view.file_type();
                out.push((
                    view.file_name().to_os_string(),
                    kind.is_some_and(fast_io::EntryKind::is_dir),
                    kind.is_some_and(fast_io::EntryKind::is_symlink),
                ));
            }
            Ok(out)
        }
        #[cfg(not(unix))]
        {
            let _ = relative;
            let mut out = Vec::new();
            for entry in std::fs::read_dir(target_path)? {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let ft = entry.file_type().ok();
                out.push((
                    entry.file_name(),
                    ft.as_ref().is_some_and(std::fs::FileType::is_dir),
                    ft.as_ref().is_some_and(std::fs::FileType::is_symlink),
                ));
            }
            Ok(out)
        }
    }

    /// Recursively removes one extraneous entry under the cap. Returns `true`
    /// when the entry was fully removed and `false` when it (or part of its
    /// subtree) was left in place because the cap was reached.
    fn remove_entry(
        &mut self,
        rel: &Path,
        path: &Path,
        is_dir: bool,
        is_symlink: bool,
    ) -> io::Result<bool> {
        if is_dir && !is_symlink {
            // Peel the directory's contents depth-first before considering the
            // directory itself (upstream delete_dir_contents, reverse order).
            let mut children = match self.scan_dir(rel, path) {
                Ok(children) => children,
                Err(e) => {
                    if let Some(err) = fail_loud_unlink_error(e) {
                        return Err(err);
                    }
                    self.io_err_bits |= crate::generator::io_error_flags::IOERR_GENERAL;
                    return Ok(false);
                }
            };
            children.sort_by(|a, b| f_name_cmp_full(Path::new(&a.0), a.1, Path::new(&b.0), b.1));
            children.reverse();

            let mut all_removed = true;
            for (child_name, child_is_dir, child_is_symlink) in children {
                let child_rel = rel.join(&child_name);
                let child_path = path.join(&child_name);
                if !self.remove_entry(&child_rel, &child_path, child_is_dir, child_is_symlink)? {
                    all_removed = false;
                }
            }

            if !all_removed {
                // upstream: delete.c:117 - one notice per non-empty directory,
                // not counted and not an I/O error.
                info_log!(
                    Nonreg,
                    1,
                    "cannot delete non-empty directory: {}",
                    path.display().to_string().replace('\\', "/")
                );
                return Ok(false);
            }

            if self.deleted >= self.limit {
                self.skipped = self.skipped.saturating_add(1);
                return Ok(false);
            }
            return self.unlink_leaf(rel, path, true, false);
        }

        if self.deleted >= self.limit {
            self.skipped = self.skipped.saturating_add(1);
            return Ok(false);
        }
        self.unlink_leaf(rel, path, false, is_symlink)
    }

    /// Issues the actual removal for one leaf, updates the stats/itemize on
    /// success, and applies the receiver's error policy on failure. Returns
    /// `true` on a successful (or vanished) removal.
    fn unlink_leaf(
        &mut self,
        rel: &Path,
        path: &Path,
        is_dir: bool,
        is_symlink: bool,
    ) -> io::Result<bool> {
        let result = self.raw_unlink(rel, path, is_dir);
        match result {
            Ok(()) => {
                self.deleted = self.deleted.saturating_add(1);
                if is_dir {
                    self.combined.dirs = self.combined.dirs.saturating_add(1);
                } else if is_symlink {
                    self.combined.symlinks = self.combined.symlinks.saturating_add(1);
                } else {
                    self.combined.files = self.combined.files.saturating_add(1);
                }
                // upstream: log.c:log_delete() emits one line per deleted item.
                if is_dir {
                    info_log!(Del, 1, "deleting {}/", rel.display());
                } else {
                    info_log!(Del, 1, "deleting {}", rel.display());
                }
                if self.emit_itemize {
                    let line = format!("*deleting   {}\n", rel.display());
                    let _ = self.writer.send_msg_info(line.as_bytes());
                }
                Ok(true)
            }
            Err(e) => {
                debug_log!(Del, 1, "failed to delete {}: {}", path.display(), e);
                if let Some(err) = fail_loud_unlink_error(e) {
                    return Err(err);
                }
                // EACCES / NotFound: upstream leaves the entry and continues.
                Ok(false)
            }
        }
    }

    /// Performs the unlink/rmdir syscall, anchored through the sandbox helper
    /// on Unix.
    fn raw_unlink(&self, rel: &Path, path: &Path, is_dir: bool) -> io::Result<()> {
        #[cfg(unix)]
        {
            let flags = if is_dir {
                fast_io::UnlinkFlags::Dir
            } else {
                fast_io::UnlinkFlags::File
            };
            fast_io::unlink_via_sandbox_or_fallback(
                self.sandbox.map(|a| &**a),
                self.dest_dir,
                rel,
                path,
                flags,
            )
        }
        #[cfg(not(unix))]
        {
            let _ = rel;
            if is_dir {
                std::fs::remove_dir(path)
            } else {
                std::fs::remove_file(path)
            }
        }
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
fn classify_scan_error(e: io::Error) -> (DeleteStats, Vec<DeletedEntry>, Option<io::Error>) {
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

    fn entry(rel: &str, is_dir: bool) -> DeletedEntry {
        DeletedEntry {
            rel: PathBuf::from(rel),
            is_dir,
        }
    }

    fn rels(entries: &[DeletedEntry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| e.rel.to_string_lossy().into_owned())
            .collect()
    }

    /// #517: within a single directory the deletion stream comes out in
    /// descending `f_name_cmp` order (upstream `generator.c:delete_in_dir()`
    /// iterates its ascending-sorted dirlist in reverse). Verified against
    /// `rsync 3.4.4 -rii --delete` on a flat directory of five files, which
    /// emits `z, m, c, b, a`.
    #[test]
    fn order_deletions_single_dir_is_descending() {
        // Supplied scrambled to prove ordering does not depend on input order.
        let entries = vec![
            entry("m.txt", false),
            entry("a.txt", false),
            entry("z.txt", false),
            entry("c.txt", false),
            entry("b.txt", false),
        ];
        let ordered = order_deletions_upstream(entries);
        assert_eq!(
            rels(&ordered),
            vec!["z.txt", "m.txt", "c.txt", "b.txt", "a.txt"],
        );
    }

    /// #517: directories are processed in ascending `f_name_cmp` order (the
    /// generator visits the file list ascending, one `delete_in_dir()` per
    /// directory), and within each the entries descend. A doomed subdir in
    /// the root's group is emitted (as a whole, one line) in the root scan.
    /// Models the `rsync 3.4.4 -rii --delete` layout:
    ///   root: `root_extra.txt`, doomed dir `doomed`, kept dir `keep`
    ///   keep/: `extra1.txt`, `extra2.txt`
    /// which upstream emits (minus the doomed subtree lines this pass folds
    /// into the whole-dir removal) as:
    ///   doomed/, root_extra.txt, keep/extra2.txt, keep/extra1.txt
    #[test]
    fn order_deletions_dirs_ascending_entries_descending() {
        let entries = vec![
            entry("keep/extra1.txt", false),
            entry("root_extra.txt", false),
            entry("keep/extra2.txt", false),
            entry("doomed", true),
        ];
        let ordered = order_deletions_upstream(entries);
        assert_eq!(
            rels(&ordered),
            vec![
                "doomed",
                "root_extra.txt",
                "keep/extra2.txt",
                "keep/extra1.txt",
            ],
        );
    }

    /// The ordering is deterministic: two independently shuffled inputs
    /// yield the identical sequence. This is the core #517 property -
    /// `HashMap`-keyed scan order must not leak into the emitted
    /// `deleting`/`*deleting` stream.
    #[test]
    fn order_deletions_is_deterministic() {
        let build = || {
            vec![
                entry("dir/b", false),
                entry("z_top", false),
                entry("dir/a", false),
                entry("a_top", false),
                entry("dir/sub", true),
                entry("dir/c", false),
            ]
        };
        let first = order_deletions_upstream(build());
        let mut shuffled = build();
        shuffled.reverse();
        let second = order_deletions_upstream(shuffled);
        assert_eq!(rels(&first), rels(&second));
        // And the concrete order: root group descending (z_top, a_top),
        // then dir group descending (sub is a dir so sorts after files -
        // ascending [a, b, c, sub] reversed = sub, c, b, a).
        assert_eq!(
            rels(&first),
            vec!["z_top", "a_top", "dir/sub", "dir/c", "dir/b", "dir/a",],
        );
    }
}
