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

/// Upstream `MAXPATHLEN` (`rsync.h`): a deleted name is only sent as a
/// `MSG_DELETED` frame when `len < MAXPATHLEN` (`log.c:866`); longer names fall
/// back to the local render path.
const MAXPATHLEN: usize = 4096;

/// Routes one deletion notification the way upstream `log_delete()` does.
///
/// A server generator (`server_mode`, i.e. `am_server`) at protocol >= 29 sends
/// the raw name to the client as a `MSG_DELETED` frame and prints nothing
/// locally; the client formats and gates the `deleting`/`*deleting` line. Every
/// other side (a local or client-side receiver) renders directly, exactly as
/// before.
///
/// # Upstream Reference
///
/// - `log.c:845-875` - `log_delete()`: `am_server && protocol_version >= 29 &&
///   len < MAXPATHLEN` emits `send_msg(MSG_DELETED, fname, len, ...)`, otherwise
///   `log_formatted(FCLIENT, "deleting %n" | stdout_format, ...)`.
/// - `log.c:867-868` - a directory bumps `len` to include its trailing NUL.
fn emit_delete_notification<W: crate::writer::MsgInfoSender + ?Sized>(
    writer: &mut W,
    rel: &Path,
    is_dir: bool,
    server_mode: bool,
    protocol: u8,
    emit_itemize: bool,
) {
    if server_mode && protocol >= 29 {
        let name = path_wire_bytes(rel);
        if name.len() < MAXPATHLEN {
            // upstream: log.c:867-868 - a directory carries a trailing NUL so
            // the reader (io.c:1616) can distinguish it from a regular file.
            let mut payload = name;
            if is_dir {
                payload.push(0);
            }
            let _ = writer.send_msg_deleted(&payload);
            return;
        }
    }
    // upstream: log.c:872-874 - a local/client receiver renders "deleting %n"
    // directly; %n (log.c:633-641) appends a trailing slash for a directory.
    if is_dir {
        info_log!(Del, 1, "deleting {}/", rel.display());
    } else {
        info_log!(Del, 1, "deleting {}", rel.display());
    }
    if emit_itemize {
        // upstream: log.c:log_delete() emits the "*deleting" itemize row when
        // --itemize-changes is active. The name is rendered through `%n`
        // (log.c:633-641), which appends a trailing slash for a directory, so
        // the itemize row carries the same slash the plain "deleting %n" line
        // above does.
        let line = if is_dir {
            format!("*deleting   {}/\n", rel.display())
        } else {
            format!("*deleting   {}\n", rel.display())
        };
        let _ = writer.send_msg_info(line.as_bytes());
    }
}

/// Returns the on-the-wire bytes of a deletion-root-relative name, matching the
/// raw `fname` upstream passes to `send_msg(MSG_DELETED, ...)` (`log.c:869`).
fn path_wire_bytes(rel: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        rel.as_os_str().as_bytes().to_vec()
    }
    #[cfg(not(unix))]
    {
        rel.to_string_lossy().into_owned().into_bytes()
    }
}

/// A single deleted destination entry carried out of the parallel scan
/// workers so its `deleting`/`*deleting` line can be emitted in upstream's
/// deterministic per-directory sorted order rather than the hash-random
/// order the `HashMap`-keyed scan set would otherwise produce.
#[derive(Debug)]
pub(in crate::receiver) struct DeletedEntry {
    /// Path relative to the deletion root, as printed by upstream
    /// `log_delete()` (top-level entries are bare names, not `./name`).
    rel: PathBuf,
    /// Whether the entry is a directory. Directories sort after files at a
    /// given level (upstream `t_PATH`) and print with a trailing slash.
    is_dir: bool,
    /// Whether the entry is a symlink. Carried so the deferred `--delete-delay`
    /// executor can classify the removal into `DeleteStats.symlinks`, matching
    /// the inline per-type counting the immediate pass performs from `read_dir`.
    is_symlink: bool,
}

/// Orders deleted entries to match upstream's observable delete stream.
///
/// Upstream's generator walks the file list ascending by `f_name_cmp` and
/// calls `generator.c:delete_in_dir()` once per scanned directory in that
/// order. Each `delete_in_dir()` scans its directory's dirlist - sorted
/// ascending by `f_name_cmp` (files before dirs at a given level) - and
/// iterates it in reverse (`for (j = dirlist->used; j--; )`). An extraneous
/// subdirectory is removed by `delete.c:delete_item()`, which recurses through
/// `delete_dir_contents()` and logs every descendant (in the same
/// reverse-dirlist order) *before* the directory's own `rmdir`/`log_delete`.
/// The observable stream is therefore a depth-first, post-order walk:
///
/// - scanned directories in ascending `f_name_cmp` order,
/// - within each, entries in descending `f_name_cmp` order,
/// - each extraneous directory expanded to its recorded descendants (same
///   rules, recursively) immediately before the directory's own line.
///
/// The parallel scan records a flat, unordered set (each doomed directory plus
/// every descendant enumerated in `record_doomed_dir_descendants`). This
/// reconstructs the tree from the recorded rel paths (a subtree root is an
/// entry whose parent is a scanned directory, absent from the set; a descendant
/// is an entry whose parent is itself a recorded doomed directory) and emits it
/// in the depth-first order above, so the result is upstream-matching
/// regardless of the parallel scan/unlink order.
///
/// # Upstream Reference
///
/// - `generator.c:delete_in_dir()` - sorted dirlist, reverse iteration
/// - `delete.c:80-109 delete_dir_contents()` - recurse into a doomed dir,
///   logging each descendant before the enclosing directory
/// - `generator.c:2328` generate_files loop - one delete_in_dir() per
///   directory, in ascending file-list order
/// - `flist.c:fsort()` / `f_name_cmp()` - the ascending comparator
fn order_deletions_upstream(entries: Vec<DeletedEntry>) -> Vec<DeletedEntry> {
    use std::collections::{BTreeMap, HashMap, HashSet};

    if entries.len() < 2 {
        return entries;
    }

    // Every recorded rel path, so a descendant recorded during a recursive
    // directory removal (parent in the set) can be told apart from a subtree
    // root (parent is a scanned directory, absent from the set).
    let deleted: HashSet<PathBuf> = entries.iter().map(|e| e.rel.clone()).collect();

    let nodes = entries;
    // A BTreeMap keyed on the f_name_cmp order of the scanned (parent)
    // directory keeps the subtree roots in the ascending order upstream's
    // generator visits them; children_of maps a doomed directory to the
    // descendants recorded under it.
    let mut children_of: HashMap<PathBuf, Vec<usize>> = HashMap::new();
    let mut roots: BTreeMap<DirKey, Vec<usize>> = BTreeMap::new();
    for (i, e) in nodes.iter().enumerate() {
        let parent = e.rel.parent().map(Path::to_path_buf).unwrap_or_default();
        if deleted.contains(&parent) {
            children_of.entry(parent).or_default().push(i);
        } else {
            roots.entry(DirKey(parent)).or_default().push(i);
        }
    }

    let mut order: Vec<usize> = Vec::with_capacity(nodes.len());
    for (_scan_dir, mut group) in roots {
        sort_reverse_dirlist(&mut group, &nodes);
        for idx in group {
            push_post_order(idx, &nodes, &children_of, &mut order);
        }
    }

    let mut slots: Vec<Option<DeletedEntry>> = nodes.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|i| slots[i].take().expect("each index is visited exactly once"))
        .collect()
}

/// Sorts entry indices into upstream's reverse-dirlist order: ascending
/// `f_name_cmp` (files before dirs at a level) then reversed, so a directory's
/// siblings are visited highest-first, matching `delete_in_dir()` iterating its
/// sorted dirlist with `for (j = dirlist->used; j--; )`.
fn sort_reverse_dirlist(idxs: &mut [usize], nodes: &[DeletedEntry]) {
    idxs.sort_by(|&a, &b| {
        f_name_cmp_full(
            &nodes[a].rel,
            nodes[a].is_dir,
            &nodes[b].rel,
            nodes[b].is_dir,
        )
    });
    idxs.reverse();
}

/// Appends one entry and its subtree in upstream emission order: a directory's
/// recorded descendants (reverse-dirlist order, recursively) come out before
/// the directory itself, mirroring `delete.c:delete_dir_contents()` which logs
/// each child via `delete_item()` before the enclosing directory's own line.
fn push_post_order(
    idx: usize,
    nodes: &[DeletedEntry],
    children_of: &std::collections::HashMap<PathBuf, Vec<usize>>,
    out: &mut Vec<usize>,
) {
    if let Some(children) = children_of.get(&nodes[idx].rel) {
        let mut children = children.clone();
        sort_reverse_dirlist(&mut children, nodes);
        for child in children {
            push_post_order(child, nodes, children_of, out);
        }
    }
    out.push(idx);
}

/// Records every descendant of a doomed directory as its own [`DeletedEntry`]
/// so the parallel delete pass itemizes each removed child, not just the
/// directory. Read-only: the caller performs the actual (wholesale) removal.
/// Entries may be pushed in any order - [`order_deletions_upstream`] re-derives
/// upstream's children-before-directory emission order from the rel paths.
///
/// # Upstream Reference
///
/// - `delete.c:80-109 delete_dir_contents()` - recurses into a doomed
///   directory; a subdirectory is expanded before it is itself deleted.
/// - `delete.c:178-181 delete_item()` -> `log.c:845 log_delete()` - one
///   `deleting` line per removed entry.
fn record_doomed_dir_descendants(
    #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    dest_dir: &Path,
    rel: &Path,
    path: &Path,
    out: &mut Vec<DeletedEntry>,
) {
    let children = match read_dir_children(
        #[cfg(unix)]
        sandbox,
        dest_dir,
        rel,
        path,
    ) {
        Ok(children) => children,
        // A read failure leaves the itemize list short but never blocks the
        // wholesale removal below; upstream likewise only logs what
        // get_dirlist() could enumerate.
        Err(_) => return,
    };
    for (name, is_dir, is_symlink) in children {
        let child_rel = rel.join(&name);
        let child_path = path.join(&name);
        if is_dir && !is_symlink {
            record_doomed_dir_descendants(
                #[cfg(unix)]
                sandbox,
                dest_dir,
                &child_rel,
                &child_path,
                out,
            );
        }
        out.push(DeletedEntry {
            rel: child_rel,
            is_dir,
            is_symlink,
        });
    }
}

/// Lists the immediate children of a directory as `(name, is_dir, is_symlink)`,
/// routing through the sandbox-anchored listing on Unix (SEC-1.q2 audit row #5)
/// and falling back to `std::fs::read_dir` otherwise. `dest_dir`/`rel` are the
/// sandbox base and the base-relative path of `path`; `rel == "."` lists the
/// base itself.
fn read_dir_children(
    #[cfg(unix)] sandbox: Option<&fast_io::DirSandbox>,
    dest_dir: &Path,
    rel: &Path,
    path: &Path,
) -> io::Result<Vec<(std::ffi::OsString, bool, bool)>> {
    let mut out = Vec::new();
    #[cfg(unix)]
    {
        let scan_rel: &Path = if rel.as_os_str() == "." {
            Path::new("")
        } else {
            rel
        };
        let iter = fast_io::read_dir_via_sandbox_or_fallback(sandbox, dest_dir, scan_rel, path)?;
        for view in iter.flatten() {
            let kind = view.file_type();
            out.push((
                view.file_name().to_os_string(),
                kind.is_some_and(fast_io::EntryKind::is_dir),
                kind.is_some_and(fast_io::EntryKind::is_symlink),
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (dest_dir, rel);
        for entry in std::fs::read_dir(path)?.flatten() {
            let ft = entry.file_type().ok();
            out.push((
                entry.file_name(),
                ft.as_ref().is_some_and(std::fs::FileType::is_dir),
                ft.as_ref().is_some_and(std::fs::FileType::is_symlink),
            ));
        }
    }
    Ok(out)
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
    /// Deletes extraneous destination entries immediately: scan, unlink, and emit
    /// the `deleting`/`*deleting` lines in one pass. Used by `--delete-before`,
    /// `--delete-during`, and `--delete-after` (and a capped/`--one-file-system`
    /// `--delete-delay`, which cannot defer through the serial executor).
    ///
    /// Returns `(stats, limit_exceeded, io_error_bits)`.
    pub(in crate::receiver) fn delete_extraneous_files<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
    ) -> io::Result<(DeleteStats, bool, i32)> {
        let (stats, limit_exceeded, io_bits, _victims) = self.run_delete_scan(
            dest_dir,
            #[cfg(unix)]
            sandbox,
            writer,
            false,
        )?;
        Ok((stats, limit_exceeded, io_bits))
    }

    /// Decides the `--delete-delay` victim set during the transfer walk WITHOUT
    /// unlinking or emitting anything, returning the entries in upstream's
    /// deterministic emission order for a later [`execute_delayed_deletions`].
    ///
    /// upstream: generator.c:157 `remember_delete()` records each doomed entry
    /// into the delete-delay buffer from inside `delete_in_dir()` while the
    /// destination `.rsync-filter` merge files are in their DURING-time state;
    /// only the physical unlink is postponed to `do_delayed_deletions()`
    /// (generator.c:2419). Deciding here - not at flush time - keeps the victim
    /// set identical to `--delete-during` even though the unlink runs late.
    ///
    /// [`execute_delayed_deletions`]: Self::execute_delayed_deletions
    pub(in crate::receiver) fn collect_delayed_deletions<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
    ) -> io::Result<(Vec<DeletedEntry>, i32)> {
        let (_stats, _limit, io_bits, victims) = self.run_delete_scan(
            dest_dir,
            #[cfg(unix)]
            sandbox,
            writer,
            true,
        )?;
        Ok((victims, io_bits))
    }

    /// Executes a previously [collected](Self::collect_delayed_deletions)
    /// `--delete-delay` victim set: unlinks each entry and emits its
    /// `deleting`/`*deleting` line, in the order the collector fixed.
    ///
    /// upstream: generator.c:2419 `do_delayed_deletions()` runs after the whole
    /// transfer completes and calls `delete_item()` (which logs and unlinks) for
    /// each remembered victim. Counting happens here, at unlink time, matching
    /// upstream's `stats.deleted_*` increments inside `delete_item`.
    pub(in crate::receiver) fn execute_delayed_deletions<
        W: crate::writer::MsgInfoSender + ?Sized,
    >(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        victims: &[DeletedEntry],
        writer: &mut W,
    ) -> io::Result<(DeleteStats, i32)> {
        #[cfg(unix)]
        let sandbox_ref = sandbox.map(|arc| arc.as_ref());
        let mut stats = DeleteStats::new();
        let mut io_bits: i32 = 0;
        let emit_itemize = self.should_emit_itemize();
        let server_mode = !self.config.connection.client_mode;
        let protocol = self.protocol.as_u8();
        for entry in victims {
            let path = dest_dir.join(&entry.rel);
            let result = if entry.is_dir {
                // upstream: delete.c:delete_item() -> delete_dir_contents() for a
                // directory victim; recursive removal mirrors the immediate pass.
                #[cfg(unix)]
                {
                    fast_io::recursive_unlinkat_via_sandbox_or_fallback(
                        sandbox_ref,
                        dest_dir,
                        &entry.rel,
                        &path,
                    )
                }
                #[cfg(not(unix))]
                {
                    std::fs::remove_dir_all(&path)
                }
            } else {
                #[cfg(unix)]
                {
                    fast_io::unlink_via_sandbox_or_fallback(
                        sandbox_ref,
                        dest_dir,
                        &entry.rel,
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
                    if entry.is_dir {
                        stats.dirs += 1;
                    } else if entry.is_symlink {
                        stats.symlinks += 1;
                    } else {
                        stats.files += 1;
                    }
                    emit_delete_notification(
                        writer,
                        &entry.rel,
                        entry.is_dir,
                        server_mode,
                        protocol,
                        emit_itemize,
                    );
                }
                Err(e) => {
                    debug_log!(Del, 1, "failed to delete {}: {}", path.display(), e);
                    if fail_loud_unlink_error(e).is_some() {
                        io_bits |= crate::generator::io_error_flags::IOERR_GENERAL;
                    }
                }
            }
        }
        Ok((stats, io_bits))
    }

    /// Reports whether the delete pass routes through the serial, leaf-granular
    /// executor (`--max-delete` cap or `--one-file-system` boundary) rather than
    /// the parallel fast path.
    ///
    /// `--delete-delay` can only collect-then-execute on the parallel path; a
    /// capped or one-file-system delay run enforces its cap/boundary inline, so
    /// it stays on the immediate early pass. Mirrors the engine crate's
    /// `delay_decides_during` gate, which likewise excludes `--max-delete`.
    pub(in crate::receiver) fn delete_pass_uses_serial_executor(&self) -> bool {
        #[cfg(unix)]
        {
            self.config.deletion.max_delete.is_some() || self.config.flags.one_file_system >= 1
        }
        #[cfg(not(unix))]
        {
            self.config.deletion.max_delete.is_some()
        }
    }

    /// Scans the destination for extraneous entries and, unless `collect_only`,
    /// unlinks them and emits their `deleting`/`*deleting` lines.
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
    /// When `collect_only` is true the pass records each victim but performs no
    /// unlink and no emission, returning the ordered victim list for a deferred
    /// `--delete-delay` execution (upstream `remember_delete()`).
    ///
    /// Returns `(stats, limit_exceeded, io_error_bits, ordered_victims)`.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:delete_in_dir()` - scans one directory, removes unlisted entries
    /// - `generator.c:do_delete_pass()` - full tree walk deletion sweep
    /// - `main.c:1367` - `deletion_count >= max_delete` check
    /// - `exclude.c:check_filter()` - is_excluded() before deletion
    fn run_delete_scan<W: crate::writer::MsgInfoSender + ?Sized>(
        &self,
        dest_dir: &Path,
        #[cfg(unix)] sandbox: Option<&std::sync::Arc<fast_io::DirSandbox>>,
        writer: &mut W,
        collect_only: bool,
    ) -> io::Result<(DeleteStats, bool, i32, Vec<DeletedEntry>)> {
        use std::collections::{HashMap, HashSet};
        use std::path::PathBuf;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU64, Ordering};

        // upstream: generator.c:295-296 - delete_in_dir() pokes a keepalive
        // (`if (allowed_lull) maybe_send_keepalive(...)`) as it begins scanning a
        // directory, so a remote sender does not time out while the generator is
        // busy sweeping the destination tree without writing to the socket. oc
        // scans directories in parallel workers that cannot reach the writer, so
        // the poke is emitted at the pass boundaries here (entry, and again after
        // the parallel scan) plus per-directory in the serial capped executor.
        // A strict no-op unless --timeout is set (allowed_lull None).
        writer.maybe_send_keepalive()?;

        let max_delete = self.config.deletion.max_delete;

        // --one-file-system boundary device. When `-x` is active the transfer
        // root's device id pins the filesystem the delete pass may touch: a
        // destination directory whose device differs is a mount point (or a
        // foreign filesystem) that upstream refuses to delete and that pins its
        // parent as non-empty. Captured once here from the top-level
        // destination directory, then compared against each directory entry in
        // the serial executor below.
        //
        // upstream: generator.c:310-321 delete_in_dir() tracks `filesystem_dev`
        // and flist.c:1344-1356 sets FLAG_MOUNT_DIR on a dest dirlist entry
        // whose `st_dev` differs from that boundary; delete_in_dir() then skips
        // it ("cannot delete mount point") and delete.c:89-97
        // delete_dir_contents() pins the parent directory as non-empty.
        #[cfg(unix)]
        let boundary_dev: Option<u64> = if self.config.flags.one_file_system >= 1 {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(dest_dir).ok().map(|m| m.dev())
        } else {
            None
        };

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
        // Route to the serial, leaf-granular executor when either `--max-delete`
        // is set (cap enforcement) or `--one-file-system` is active (mount-point
        // boundary enforcement). Both need the depth-first, per-entry decision
        // the parallel wholesale-remove fast path cannot express: the cap must
        // count individual leaves, and the mount check must preserve a foreign
        // filesystem nested anywhere inside a doomed subtree while pinning its
        // parent. When neither is set the parallel fast path runs unchanged, so
        // the common (no `-x`, no cap) delete pass pays zero extra cost.
        #[cfg(unix)]
        let use_serial_executor = max_delete.is_some() || boundary_dev.is_some();
        #[cfg(not(unix))]
        let use_serial_executor = max_delete.is_some();
        if use_serial_executor {
            // A one-file-system run without `--max-delete` has no cap, so an
            // unreachable u64::MAX sentinel keeps the executor's guard-before-
            // delete logic intact without ever tripping the limit warning.
            let limit = max_delete.unwrap_or(u64::MAX);
            // The serial executor unlinks inline, so a `--delete-delay` run that
            // reaches it cannot defer; the caller keeps such a run on the
            // immediate early pass (see `delete_pass_uses_serial_executor`), and
            // `collect_only` is never set here.
            debug_assert!(
                !collect_only,
                "delayed collection never uses the serial executor"
            );
            let (stats, limit_exceeded, io_bits) = self.delete_extraneous_files_capped(
                dest_dir,
                &dir_children,
                &dirs_to_scan,
                deletion_chain,
                needs_perdir_merge,
                #[cfg(unix)]
                sandbox,
                writer,
                limit,
                #[cfg(unix)]
                boundary_dev,
            )?;
            return Ok((stats, limit_exceeded, io_bits, Vec::new()));
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
                        // Deletion-root-relative path of this entry, rooted at
                        // the receiver's sandbox base (`dest_dir_owned`). Top-
                        // level entries are bare names ("delete.txt"), not
                        // "./delete.txt" (upstream log_delete, delete.c:180).
                        let entry_rel: PathBuf = if dir_relative.as_os_str() == "." {
                            PathBuf::from(&name)
                        } else {
                            dir_relative.join(&name)
                        };

                        // upstream: delete.c:80-109 delete_dir_contents() logs
                        // (via delete_item()->log_delete()) every descendant of
                        // a doomed directory, children first, before the
                        // directory's own line. The parallel fast path removes
                        // the subtree wholesale below, so enumerate the
                        // descendants here (read-only) and record each as its
                        // own deletion; order_deletions_upstream re-derives the
                        // children-before-directory emission order. Only a real
                        // directory is walked - a symlink (even to a directory)
                        // is a leaf. Empty for files, so the common path is
                        // unchanged.
                        let mut subtree: Vec<DeletedEntry> = Vec::new();
                        if is_dir && !is_symlink {
                            record_doomed_dir_descendants(
                                #[cfg(unix)]
                                sandbox_ref,
                                &dest_dir_owned,
                                &entry_rel,
                                &path,
                                &mut subtree,
                            );
                        }

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

                        // upstream: generator.c:345 `delete_during == 2` records
                        // the victim via remember_delete() and defers the unlink;
                        // in collect_only mode the worker only records the entry so
                        // the physical removal runs later in do_delayed_deletions().
                        let result = if collect_only {
                            Ok(())
                        } else if is_dir {
                            // SEC-1.q2 audit row #6
                            #[cfg(unix)]
                            {
                                fast_io::recursive_unlinkat_via_sandbox_or_fallback(
                                    sandbox_ref,
                                    &dest_dir_owned,
                                    &entry_rel,
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
                                    &entry_rel,
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
                                // Count and record every enumerated descendant
                                // (empty unless this entry is a doomed dir),
                                // then the entry itself. upstream: delete_item()
                                // bumps stats.deleted_* once per removed entry.
                                for descendant in &subtree {
                                    if descendant.is_dir {
                                        stats.dirs += 1;
                                    } else if descendant.is_symlink {
                                        stats.symlinks += 1;
                                    } else {
                                        stats.files += 1;
                                    }
                                }
                                deleted_paths.extend(subtree);
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
                                deleted_paths.push(DeletedEntry {
                                    rel: entry_rel,
                                    is_dir,
                                    is_symlink,
                                });
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

        // upstream: generator.c:295-296 - the parallel scan above ran the
        // per-directory delete_in_dir work with no writer access, so poke the
        // keepalive here at the far edge of that silent window before the serial
        // emission loop. A strict no-op unless --timeout is set.
        writer.maybe_send_keepalive()?;

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
        // In collect_only mode nothing has been unlinked yet: skip emission so
        // the `deleting`/`*deleting` lines appear only when the deferred
        // `--delete-delay` executor actually removes each victim, matching
        // upstream where log_delete() fires from delete_item() inside
        // do_delayed_deletions() (generator.c:2419), not at remember_delete()
        // time. The ordered victim list is returned for that later execution.
        if !collect_only {
            let emit_itemize = self.should_emit_itemize();
            let server_mode = !self.config.connection.client_mode;
            let protocol = self.protocol.as_u8();
            for entry in &all_deleted {
                // upstream: log.c:845 log_delete() - a server generator forwards
                // the raw name as MSG_DELETED; a local/client receiver renders
                // "deleting %n" directly (%n appends a trailing slash for dirs).
                emit_delete_notification(
                    writer,
                    &entry.rel,
                    entry.is_dir,
                    server_mode,
                    protocol,
                    emit_itemize,
                );
            }
        }

        // Limit is exceeded when we had candidates beyond the allowed count.
        let total_deletions = u64::from(combined.files)
            + u64::from(combined.dirs)
            + u64::from(combined.symlinks)
            + u64::from(combined.devices)
            + u64::from(combined.specials);
        let limit_exceeded = max_delete.is_some_and(|limit| total_deletions >= limit);

        Ok((combined, limit_exceeded, io_err_bits, all_deleted))
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
        #[cfg(unix)] boundary_dev: Option<u64>,
    ) -> io::Result<(DeleteStats, bool, i32)> {
        let mut state = CappedDeleteState {
            #[cfg(unix)]
            dest_dir,
            #[cfg(unix)]
            sandbox,
            #[cfg(unix)]
            boundary_dev,
            limit,
            deleted: 0,
            skipped: 0,
            combined: DeleteStats::new(),
            io_err_bits: 0,
            emit_itemize: self.should_emit_itemize(),
            server_mode: !self.config.connection.client_mode,
            protocol: self.protocol.as_u8(),
            writer,
        };

        for dir_relative in dirs_to_scan {
            // upstream: generator.c:295-296 - poke a keepalive at the start of
            // each per-directory scan, matching delete_in_dir()'s cadence
            // exactly on this serial path. A strict no-op unless --timeout is set.
            state.writer.maybe_send_keepalive()?;
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
    /// `--one-file-system` boundary device (the transfer root's `st_dev`).
    /// `Some` only when `-x` is active; a directory entry whose device differs
    /// is a mount point that must be preserved and pins its parent as
    /// non-empty. See [`crosses_mount_boundary`].
    #[cfg(unix)]
    boundary_dev: Option<u64>,
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
    /// `true` when this receiver is a server generator (`am_server`) and must
    /// forward deletions to the client as `MSG_DELETED` rather than render them.
    server_mode: bool,
    /// Negotiated protocol version, gating the `>= 29` `MSG_DELETED` path.
    protocol: u8,
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
            // upstream: generator.c:331-336 delete_in_dir() skips a dest dir
            // flagged FLAG_MOUNT_DIR ("cannot delete mount point"), and
            // delete.c:89-97 delete_dir_contents() treats such a nested entry
            // as pinning its parent non-empty. Under `--one-file-system` a
            // directory whose device differs from the transfer-root boundary is
            // that mount point: never delete it, and return `false` so the
            // caller leaves the parent directory in place. The check runs at
            // every recursion level, so a mount nested inside a doomed subtree
            // is preserved and pins the whole chain of ancestors.
            #[cfg(unix)]
            if let Some(boundary) = self.boundary_dev {
                use std::os::unix::fs::MetadataExt;
                if let Ok(meta) = std::fs::symlink_metadata(path)
                    && crosses_mount_boundary(boundary, meta.dev())
                {
                    info_log!(Mount, 1, "cannot delete mount point: {}", rel.display());
                    return Ok(false);
                }
            }
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
                // upstream: log.c:log_delete() emits one line per deleted item -
                // forwarded as MSG_DELETED on a server generator, rendered
                // directly otherwise.
                emit_delete_notification(
                    self.writer,
                    rel,
                    is_dir,
                    self.server_mode,
                    self.protocol,
                    self.emit_itemize,
                );
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

/// Whether a destination directory sits on a different filesystem than the
/// transfer root, i.e. is a mount point that `--one-file-system` must protect.
///
/// Pure device-id comparison, dependency-inverted from the `lstat` call site so
/// the mount-boundary decision is unit-testable with synthetic `st_dev` values
/// (mounting a real filesystem in a test is impractical). A `true` result means
/// the entry crosses the boundary and must be preserved; a `false` result means
/// it is on the same filesystem and is an ordinary deletion candidate.
///
/// # Upstream Reference
///
/// - `flist.c:1344` - `one_file_system && st.st_dev != filesystem_dev` sets
///   `FLAG_MOUNT_DIR` on the dest dirlist entry.
/// - `generator.c:331` - `delete_in_dir()` skips a `FLAG_MOUNT_DIR` directory.
#[cfg(unix)]
fn crosses_mount_boundary(boundary_dev: u64, entry_dev: u64) -> bool {
    entry_dev != boundary_dev
}

/// Tests for the server-vs-local routing of a deletion notification, encoding
/// the upstream `log.c:866-874` gate: a server generator at protocol >= 29
/// forwards the raw name (dir bytes + trailing NUL) as MSG_DELETED and prints
/// nothing locally; every other side renders `deleting %n` directly.
#[cfg(test)]
mod emit_notification_tests {
    use super::*;
    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};

    /// Records the frames a receiver would put on the wire.
    #[derive(Default)]
    struct RecordingWriter {
        deleted: Vec<Vec<u8>>,
        info: Vec<Vec<u8>>,
    }

    impl crate::writer::MsgInfoSender for RecordingWriter {
        fn send_msg_deleted(&mut self, data: &[u8]) -> io::Result<()> {
            self.deleted.push(data.to_vec());
            Ok(())
        }
        fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
            self.info.push(data.to_vec());
            Ok(())
        }
    }

    fn del_events() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|e| match e {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Del,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn server_generator_forwards_msg_deleted_not_local_print() {
        let mut cfg = VerbosityConfig::default();
        cfg.info.del = 1;
        init(cfg);
        let _ = drain_events();

        let mut w = RecordingWriter::default();
        // server_mode=true, proto 32: file -> raw name, dir -> name + NUL.
        emit_delete_notification(&mut w, Path::new("foo"), false, true, 32, false);
        emit_delete_notification(&mut w, Path::new("sub/bar"), true, true, 32, false);

        assert_eq!(w.deleted, vec![b"foo".to_vec(), b"sub/bar\0".to_vec()]);
        // Server prints nothing locally: the client renders from the wire.
        assert!(w.info.is_empty());
        assert!(
            del_events().is_empty(),
            "server must not emit local Del lines"
        );
    }

    #[test]
    fn local_receiver_renders_directly_no_frame() {
        let mut cfg = VerbosityConfig::default();
        cfg.info.del = 1;
        init(cfg);
        let _ = drain_events();

        let mut w = RecordingWriter::default();
        // server_mode=false: local/client receiver renders "deleting %n".
        emit_delete_notification(&mut w, Path::new("foo"), false, false, 32, false);
        emit_delete_notification(&mut w, Path::new("bar"), true, false, 32, false);

        assert!(w.deleted.is_empty(), "a local receiver sends no wire frame");
        assert_eq!(
            del_events(),
            vec!["deleting foo".to_owned(), "deleting bar/".to_owned()]
        );
    }

    /// The `-i` itemize row (`*deleting`) renders the name through upstream
    /// `%n`, so a directory carries a trailing slash exactly like the plain
    /// `deleting %n` line. Regression for the remote-pull `*deleting` rows,
    /// which dropped the directory slash (`*deleting   d` vs `*deleting   d/`).
    #[test]
    fn itemize_row_appends_directory_trailing_slash() {
        let mut cfg = VerbosityConfig::default();
        cfg.info.del = 1;
        init(cfg);
        let _ = drain_events();

        let mut w = RecordingWriter::default();
        // Client receiver, itemize active: file has no slash, dir has one.
        emit_delete_notification(&mut w, Path::new("stale.txt"), false, false, 32, true);
        emit_delete_notification(&mut w, Path::new("stale_dir"), true, false, 32, true);

        assert_eq!(
            w.info,
            vec![
                b"*deleting   stale.txt\n".to_vec(),
                b"*deleting   stale_dir/\n".to_vec(),
            ],
        );
    }

    #[test]
    fn protocol_below_29_falls_back_to_local_render() {
        let mut cfg = VerbosityConfig::default();
        cfg.info.del = 1;
        init(cfg);
        let _ = drain_events();

        let mut w = RecordingWriter::default();
        // upstream: log.c:866 - the MSG_DELETED path requires protocol >= 29.
        emit_delete_notification(&mut w, Path::new("foo"), false, true, 28, false);

        assert!(w.deleted.is_empty(), "proto 28 must not send MSG_DELETED");
        assert_eq!(del_events(), vec!["deleting foo".to_owned()]);
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

    /// Mount-point data-loss protection: under `--one-file-system` the delete
    /// pass must never remove a destination directory that lives on a different
    /// filesystem than the transfer root. Deleting such an entry would recurse
    /// across the mount boundary and destroy a mounted filesystem that is absent
    /// from the source flist - the exact `rsync -ax --delete` data-loss upstream
    /// guards against ("cannot delete mount point").
    ///
    /// The boundary decision is a pure `st_dev` comparison so it can be verified
    /// with synthetic device ids without mounting a real filesystem: an entry on
    /// the boundary device is an ordinary deletion candidate; an entry on any
    /// other device is a mount point that must be preserved.
    ///
    /// upstream: flist.c:1344 (`st.st_dev != filesystem_dev` -> FLAG_MOUNT_DIR),
    /// generator.c:331 (delete_in_dir skips it).
    #[test]
    fn mount_boundary_predicate_preserves_foreign_device_entries() {
        const ROOT_DEV: u64 = 0x10;

        // Same device as the transfer root: on-filesystem, safe to delete.
        assert!(
            !crosses_mount_boundary(ROOT_DEV, ROOT_DEV),
            "an entry on the transfer-root device must remain deletable",
        );

        // Different device: a mounted filesystem the delete pass must preserve.
        assert!(
            crosses_mount_boundary(ROOT_DEV, 0x20),
            "an entry on a foreign device is a mount point and must be preserved",
        );
        assert!(
            crosses_mount_boundary(ROOT_DEV, 0),
            "device 0 still differs from the boundary and must be preserved",
        );
    }

    fn entry(rel: &str, is_dir: bool) -> DeletedEntry {
        DeletedEntry {
            rel: PathBuf::from(rel),
            is_dir,
            is_symlink: false,
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
    /// directory), and within each the entries descend. An *empty* doomed
    /// subdir in the root's group is a single line in the root scan.
    /// Models the `rsync 3.4.4 -rii --delete` layout:
    ///   root: `root_extra.txt`, empty doomed dir `doomed`, kept dir `keep`
    ///   keep/: `extra1.txt`, `extra2.txt`
    /// which upstream emits as:
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

    /// A non-empty doomed directory recorded with its descendants (as the
    /// parallel worker now does via `record_doomed_dir_descendants`) is
    /// emitted depth-first: each child before the directory itself, with the
    /// directory processed before a lexically-earlier top-level file.
    /// Mirrors `rsync 3.4.4 -ri --delete` on a dest holding `stale.txt` plus
    /// `stale_dir/inner.txt`, which emits:
    ///   deleting stale_dir/inner.txt
    ///   deleting stale_dir/
    ///   deleting stale.txt
    /// upstream: delete.c:80-109 delete_dir_contents() logs each child before
    /// the enclosing directory (delete.c:178-181 delete_item->log_delete).
    #[test]
    fn order_deletions_recurses_children_before_directory() {
        let entries = vec![
            entry("stale.txt", false),
            entry("stale_dir", true),
            entry("stale_dir/inner.txt", false),
        ];
        let ordered = order_deletions_upstream(entries);
        assert_eq!(
            rels(&ordered),
            vec!["stale_dir/inner.txt", "stale_dir", "stale.txt"],
        );
    }

    /// Nested doomed subdirectories recurse depth-first: the deepest entries
    /// drain first, each directory after its own contents. upstream:
    /// delete.c:98-101 recurses `delete_dir_contents()` into a child dir
    /// before `delete_item()` removes and logs it.
    #[test]
    fn order_deletions_recurses_nested_subdirs_depth_first() {
        let entries = vec![
            entry("stale_dir", true),
            entry("stale_dir/mid.txt", false),
            entry("stale_dir/sub", true),
            entry("stale_dir/sub/deep.txt", false),
        ];
        let ordered = order_deletions_upstream(entries);
        // Within stale_dir the reverse-dirlist walk visits `sub` (a dir, so it
        // sorts after the file) before `mid.txt`; `sub` expands to `deep.txt`
        // then itself; finally the top directory.
        assert_eq!(
            rels(&ordered),
            vec![
                "stale_dir/sub/deep.txt",
                "stale_dir/sub",
                "stale_dir/mid.txt",
                "stale_dir",
            ],
        );
    }

    /// `record_doomed_dir_descendants` records every descendant of a doomed
    /// directory (files and nested subdirs), flagging directories, so the
    /// parallel delete pass can itemize each removed child. The directory
    /// itself is recorded by the caller, not here.
    #[test]
    fn record_doomed_dir_descendants_records_the_whole_subtree() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let stale = base.join("stale_dir");
        std::fs::create_dir(&stale).unwrap();
        std::fs::write(stale.join("inner.txt"), b"x").unwrap();
        let sub = stale.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("deep.txt"), b"y").unwrap();

        let mut out = Vec::new();
        record_doomed_dir_descendants(None, base, Path::new("stale_dir"), &stale, &mut out);

        let mut got: Vec<String> = out
            .iter()
            .map(|e| e.rel.to_string_lossy().into_owned())
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "stale_dir/inner.txt".to_string(),
                "stale_dir/sub".to_string(),
                "stale_dir/sub/deep.txt".to_string(),
            ],
        );
        let sub_entry = out
            .iter()
            .find(|e| e.rel == Path::new("stale_dir/sub"))
            .expect("subdir recorded");
        assert!(
            sub_entry.is_dir,
            "a nested directory must be flagged is_dir"
        );
        let inner = out
            .iter()
            .find(|e| e.rel == Path::new("stale_dir/inner.txt"))
            .expect("file recorded");
        assert!(!inner.is_dir, "a file must not be flagged is_dir");
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
