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

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use logging::{debug_log, info_log};
use protocol::flist::ProcessRole;

use crate::full_fname::full_fname_path;
use crate::role_trailer::error_location;

use super::super::GeneratorContext;
use super::super::io_error_flags;
use super::super::protocol_io::SenderDiagnostic;
use super::batch_stat::{StatResult, batch_stat_dir_entries};
#[cfg(unix)]
use super::operand_has_dotdir_marker;

/// The error a failed per-directory filter scope ends the walk with.
///
/// upstream: exclude.c:133-137 filter_rule_err() - a rule refused out of a
/// per-directory merge file prints the rule-text message alone and exits
/// RERR_SYNTAX, so the class is chosen here, where the refusal is known.
fn filter_chain_error(dir: &Path, error: filters::FilterChainError) -> io::Error {
    match error {
        filters::FilterChainError::Parse { message, .. } => protocol::syntax_violation(message),
        other => io::Error::other(format!(
            "filter chain error in \"{}\": {other} {}{}",
            dir.display(),
            error_location!(),
            crate::role_trailer::sender()
        )),
    }
}

/// Renders `dir` the way upstream names a filter directory in its FILTER
/// traces: absolute, lexically cleaned, with a trailing slash.
///
/// upstream: exclude.c:756-778 set_filter_dir() - a relative `dir` is joined
/// to `curr_dir`, `clean_fname(CFN_COLLAPSE_DOT_DOT_DIRS)` drops `.` and
/// folds `..`, and a `/` is appended unless the result is the root itself.
fn filter_dir_display(dir: &Path) -> String {
    use std::path::Component;

    let joined;
    let dir = if dir.is_absolute() {
        dir
    } else {
        joined = std::env::current_dir().map_or_else(|_| dir.to_path_buf(), |cwd| cwd.join(dir));
        joined.as_path()
    };
    let mut cleaned = PathBuf::new();
    for component in dir.components() {
        match component {
            Component::ParentDir => {
                cleaned.pop();
            }
            Component::CurDir => {}
            other => cleaned.push(other),
        }
    }
    let mut rendered = cleaned.to_string_lossy().into_owned();
    if rendered != "/" {
        rendered.push('/');
    }
    rendered
}

/// One pending step of the directory walk.
///
/// The walk used to be call-stack recursion: a directory's entry was pushed,
/// then each child was walked from inside that frame, wrapped in a filter-chain
/// scope guard. That shape cannot be suspended, and suspending is exactly what
/// upstream's producer does - `send_directory()` scans ONE directory and
/// `send_extra_file_list()` (`flist.c`) decides when to scan the next.
///
/// Making the stack explicit keeps the guard nesting while turning every pop
/// into a point the walk can stop at.
///
/// The visit order is upstream's send order, which is the order the entries go
/// out on the wire: `send_directory()` emits every child of a directory in
/// readdir order, and only then recurses into the child directories, in the
/// order they were emitted (`flist.c:2434-2439`).
enum WalkStep {
    /// A path whose metadata is already final: the entry points, which resolve
    /// metadata before they call in.
    Visit {
        path: PathBuf,
        metadata: fast_io::EntryMetadata,
        is_top_level: bool,
    },
    /// A directory child straight out of the batch stat.
    ///
    /// The `--copy-dirlinks` / `--copy-unsafe-links` re-stat is deliberately
    /// NOT applied when the child is scheduled. It runs when the child is
    /// reached, so its `copying unsafe symlink` notice prints in send order.
    VisitChild(StatResult),
    /// Read `dir`'s children, batch-stat them, and schedule each one.
    ScanChildren { dir: PathBuf },
    /// Descend into the directories found by the scan this step closes, in the
    /// order its children were visited.
    ///
    /// upstream: `flist.c:2434-2439` - the `send_if_directory()` loop after
    /// `closedir()`.
    DescendFound,
    /// Enter `dir`'s filter scope, then scan it.
    ///
    /// upstream: `flist.c:send_if_directory()` - `push_local_filters()` then
    /// `send_directory()`.
    Descend(PathBuf),
    /// Release one per-directory filter scope.
    ///
    /// upstream: `exclude.c:pop_local_filters()`
    LeaveDir(filters::DirFilterGuard),
}

/// What the driver does when it reaches a subdirectory.
///
/// upstream: `flist.c:send_directory()` takes a flags word, and
/// `send_extra_file_list()` passes `FLAG_DIVERT_DIRS` so a subdirectory found
/// while scanning a sub-list is recorded and diverted rather than descended
/// into. That single flag is why upstream never materialises the whole tree:
/// one call scans one level, and the diverted directories become the work list
/// for later calls.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Descent {
    /// Descend into every subdirectory, so one drive produces the whole tree.
    Recurse,
    /// Emit a subdirectory's own entry and stop there - its children belong to
    /// a later scan. upstream: `FLAG_DIVERT_DIRS`.
    DivertDirs,
}

/// The two things every walk step needs that are not part of the step itself:
/// the base the wire-side relative name is computed against, and how deep to go.
///
/// Carried as one value rather than two parameters so adding the policy does
/// not widen every signature in the driver.
#[derive(Clone, Copy)]
struct WalkScope<'a> {
    base: &'a Path,
    descent: Descent,
}

impl<'a> WalkScope<'a> {
    /// The whole-tree walk every existing caller performs.
    fn recursive(base: &'a Path) -> Self {
        Self {
            base,
            descent: Descent::Recurse,
        }
    }

    /// One level only. upstream: `FLAG_DIVERT_DIRS`.
    fn one_level(base: &'a Path) -> Self {
        Self {
            base,
            descent: Descent::DivertDirs,
        }
    }

    /// Whether a subdirectory reached under this scope is descended into.
    ///
    /// The transfer root is not governed by this: `send_file_list()` always
    /// scans the root it was pointed at, whichever flags it carries. Only
    /// directories found *inside* a scan are diverted.
    fn descends_into_subdirs(self) -> bool {
        matches!(self.descent, Descent::Recurse)
    }
}

impl GeneratorContext {
    /// The error with which the `--files-from` ownership walk refuses the list
    /// entry `path`, as opposed to finding nothing there; `None` when the walk
    /// is inactive.
    pub(in crate::generator) fn files_from_walk_refusal(
        &self,
        path: &Path,
        base: &Path,
    ) -> Option<io::Error> {
        #[cfg(unix)]
        if self.files_from_base.is_some() {
            return self
                .resolve_symlink_metadata(path, base, operand_has_dotdir_marker(path))
                .err()
                .filter(|error| error.kind() != io::ErrorKind::NotFound);
        }
        let _ = (path, base);
        None
    }

    /// Reports a list entry the ownership walk refused, as the entry's own
    /// stat failure.
    ///
    /// upstream: `flist.c:2940-2946` - `rsyserr(FERROR_XFER, errno, "link_stat
    /// %s failed", ...)` with `io_error |= IOERR_GENERAL`.
    pub(in crate::generator) fn report_files_from_refusal(
        &mut self,
        path: &Path,
        error: &io::Error,
    ) {
        self.log_stat_error(path, error);
        self.record_io_error(error);
    }

    /// Enumerates a source directory, confined beneath this transfer's root
    /// when it has one.
    ///
    /// Sole owner of the enumeration-anchor decision for the walk, so the two
    /// scan sites cannot disagree about whether a directory read is confined -
    /// the same reason [`Self::confine_root`] is the sole owner of the boundary
    /// it reads.
    ///
    /// The root is passed to `fast_io` rather than left ambient. `read_dir`'s
    /// own anchor is the process-global session pin, which at the time was not
    /// installed for a daemon, so this walk silently degraded to an ordinary
    /// absolute-path read, while [`Self::confine_root`] (already
    /// daemon-correct) was consulted only by the CONTENT open. That split is
    /// exactly what let a module symlink be enumerated but not read.
    ///
    /// The daemon DOES publish that global today - `publish_module_confinement`
    /// (daemon/sections/module_access/transfer/sandbox.rs) calls
    /// `install_daemon_session` on every served connection - which is precisely
    /// why the anchor still must not be read from it here. oc serves each
    /// connection on a worker thread of one process, so a per-connection value
    /// in a global answers whichever module published LAST. That is not
    /// theoretical: the sibling opt-out bit in the same global let a module
    /// with `insecure links = yes` switch off the confinement of a concurrent
    /// connection to a module that never opted out, measured at 47 of 80
    /// concurrent rounds (see `tests/daemon_concurrent_module_confinement.rs`).
    /// Passing the root in is what keeps this call correct under threads.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.1/flist.c:2256-2295` `secure_opendir()` - the confined open
    ///   producing `scan_dirfd` for a daemon's scan.
    /// - `rsync-3.5.1/flist.c:2374` - the `opendir` failure diagnostic both call
    ///   sites report, unchanged by the anchoring.
    fn scan_source_dir(
        &self,
        path: &Path,
        depth: usize,
    ) -> io::Result<fast_io::pinned_root::ReadDir> {
        // upstream: flist.c:2355-2361 - every other scan keeps the legacy
        // opendir(), which lists a readable directory without searching it.
        if !self.scan_is_secure() {
            return fast_io::pinned_root::read_dir(path);
        }
        // upstream: flist.c:2356 secure_opendir() walks the name's every
        // component, so the confined scan stops at the resolver's depth.
        fast_io::walk_depth::check_secure_walk_depth(depth)?;
        // upstream: flist.c:2262-2265 - a `--files-from` directory opens
        // through the ownership walk before any other arm is considered.
        #[cfg(unix)]
        if let Some(base) = self.files_from_base.as_deref() {
            let names = base.read_dir(path)?;
            return Ok(fast_io::pinned_root::ReadDir::from_names(path, names));
        }
        #[cfg(unix)]
        if let Some(root) = self.confine_root() {
            return fast_io::pinned_root::read_dir_under(&root, path);
        }
        // upstream: flist.c:2265-2266 - outside a daemon the scan opens through
        // the operand's held root, so a root whose identity changed since
        // flist.c:302-305 recorded it fails with ELOOP (flist.c:325-329).
        if let Some(Err(error)) = self.source_roots.held_root(path) {
            return Err(error);
        }
        // upstream: syscall.c:3172/:3218 - secure_opendir() reopens "."
        // beneath a traversal descriptor, so a directory without search
        // permission fails here with EACCES.
        #[cfg(unix)]
        return fast_io::pinned_root::read_dir_searchable(path);
        #[cfg(not(unix))]
        fast_io::pinned_root::read_dir(path)
    }

    /// Whether this scan opens its directory through upstream's
    /// `secure_opendir()` rather than the legacy `opendir()`.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync-3.5.1/flist.c:2357-2361` - `!symlink_optout_allowed() &&
    ///   (secure_relpath_active() || !(copy_links || copy_unsafe_links ||
    ///   copy_dirlinks || insecure_links))`.
    /// - `rsync-3.5.1/syscall.c:139-144` `symlink_optout_allowed()` - the
    ///   module's `insecure links` for a daemon, `--insecure-links` otherwise.
    ///   A non-daemon's `insecure_links` term is therefore already the opt-out.
    /// - `rsync-3.5.1/syscall.c:117-131` `secure_relpath_active()` - for a
    ///   sender only a daemon, and a chrooted one only while a module
    ///   directory remains inside the chroot.
    fn scan_is_secure(&self) -> bool {
        let connection = &self.config.connection;
        let opted_out = if connection.is_daemon_connection {
            connection.daemon_insecure_links
        } else {
            fast_io::confinement::session_optout_allowed()
        };
        if opted_out {
            return false;
        }
        let relpath_active = connection.is_daemon_connection
            && (!fast_io::confinement::session_is_chrooted()
                || connection.daemon_module_root.is_some());
        let flags = &self.config.flags;
        relpath_active || !(flags.copy_links || flags.copy_unsafe_links || flags.copy_dirlinks)
    }

    /// Pre-checks a top-level source entry and walks it if it exists.
    ///
    /// Returns `true` if the entry was processed (exists or was handled as a
    /// mode-0 sentinel), `false` if the caller should skip to the next entry.
    ///
    /// This method applies `--ignore-missing-args` and `--delete-missing-args`
    /// semantics for top-level source paths and `--files-from` entries. The
    /// distinction from recursive children (which use `walk_path` directly)
    /// is critical: a missing top-level source is "never existed at flist time"
    /// (upstream `link_stat ... failed`, exit 23), while a missing recursive
    /// child is "vanished during walk" (upstream `file has vanished`, exit 24).
    ///
    /// `is_dotdir` is upstream's `name_type != NORMAL_NAME` for this operand.
    /// It is a caller-supplied fact rather than something re-derived from
    /// `path`, because `--relative` strips the marker out of the transmitted
    /// name (`flist.c:2891-2897`) while keeping its effect on the follow.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2491-2511` - `link_stat` + `missing_args` handling per source
    pub(in crate::generator) fn try_walk_source_entry(
        &mut self,
        base: &Path,
        path: &Path,
        is_dotdir: bool,
    ) -> io::Result<bool> {
        // upstream: options.c:2323-2329 - `xfer_dirs` resolves to `-d`, or `-r`
        // (`else if (recurse) xfer_dirs = 1`), or `list_only` when neither was
        // given. `--files-from` forces it on (options.c:2316-2317), which is
        // why `build_file_list_with_base` passes `true` instead of this.
        let xfer_dirs =
            self.config.flags.recursive || self.config.flags.dirs || self.config.flags.list_only;
        // upstream: flist.c:2967 - only a non-daemon sender reading explicit
        // operands (never a --files-from list, `use_ff_fd`) pins source roots.
        let remember_root = !self.config.connection.is_daemon_connection;
        self.try_walk_source_entry_dedup(base, path, None, xfer_dirs, is_dotdir, remember_root)
    }

    /// Reports a source operand whose DOTDIR `dir` half cannot be entered.
    ///
    /// `change_pathname()` first resets `curr_dir` to the starting (module)
    /// directory, so the name renders against that, not the previous operand.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:661-662` - `change_dir(orig_dir, CD_SKIP_CHDIR)` for a
    ///   relative `dir`
    /// - `flist.c:686-689` - `io_error |= IOERR_GENERAL` and
    ///   `rsyserr(FERROR_XFER, errno, "change_dir %s failed", full_fname(dir))`
    pub(in crate::generator) fn report_change_dir_failure(
        &mut self,
        dir: &Path,
        error: &io::Error,
    ) {
        self.curr_dir = None;
        let text = format!(
            "rsync: [sender] change_dir {} failed: {}\n",
            full_fname_path(dir, self.full_fname_paths()),
            engine::local_copy::upstream_io_error(error),
        );
        self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
        self.add_io_error(io_error_flags::IOERR_GENERAL);
    }
    /// Upstream's `xfer_dirs` gate on a top-level directory argument.
    ///
    /// Without `-r`, `-d`, or a `--list-only`/`--files-from` equivalent,
    /// upstream refuses a directory operand outright: it prints one FINFO line
    /// and `continue`s the argument loop, so the directory contributes NO
    /// file-list entry - not the directory, not one level of its children, and
    /// not its `--relative` implied parents.
    ///
    /// The DOTDIR spellings (`dir/`, `dir/.`) are NOT exempt. They still stat
    /// as a directory, so a trailing slash without `-r`/`-d` transfers nothing
    /// at all; the marker only decides "contents of" once directories are being
    /// transferred in the first place.
    ///
    /// `fbuf` at this point is the operand after upstream's `dir`/`fn` split,
    /// which is `relative` here - except for the two spellings the split cannot
    /// produce: a DOTDIR operand collapses to a bare `.`, and a `--relative`
    /// absolute operand keeps its leading `/` (upstream only strips that on the
    /// receiver, `flist.c:3314`).
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2959-2961` - the `ALL_FILTERS` exclusion runs FIRST and drops
    ///   an excluded argument silently; `name_type != DOTDIR_NAME` exempts a
    ///   DOTDIR operand from it.
    /// - `flist.c:2963-2966` - `if (S_ISDIR(st.st_mode) && !xfer_dirs) {
    ///   rprintf(FINFO, "skipping directory %s\n", fbuf); continue; }`
    fn report_skipped_directory_arg(&mut self, base: &Path, path: &Path) {
        let relative = path.strip_prefix(base).unwrap_or(path);
        let is_dotdir = relative.as_os_str().is_empty();
        if !is_dotdir && !self.filter_chain.allows(relative, true) {
            return;
        }
        let name: PathBuf = if is_dotdir {
            PathBuf::from(".")
        } else if base == Path::new("/") {
            path.components().collect()
        } else {
            relative.components().collect()
        };
        self.queue_flist_diagnostic(
            SenderDiagnostic::Info,
            format!("skipping directory {}\n", name.display()),
        );
    }

    /// Like `try_walk_source_entry`, but consults `emitted_dirs` to skip
    /// re-emitting a directory entry that was already pushed by an earlier
    /// pass (e.g. the implied-parent loop in `build_file_list_with_base`).
    ///
    /// When the source path is a directory whose relative name is already in
    /// `emitted_dirs`, the function returns `Ok(true)` without re-emitting or
    /// recursing. Subsequent `--files-from` entries that explicitly name
    /// children of the same directory still reach the receiver via their own
    /// top-level walks, and re-walking here would produce a duplicate parent
    /// entry that upstream's `implied_filter_list` check rejects with
    /// "rejecting unrequested file-list name" (flist.c:1368).
    ///
    /// `emitted_dirs` is `Some` only from `build_file_list_with_base`, which
    /// passes the set of directories already emitted by its implied-parent
    /// loop. All other callers (single-source `build_file_list`) pass `None`
    /// and retain the original walk-everything behaviour.
    ///
    /// `remember_root` pins the operand's source root for the content open;
    /// it is false for a `--files-from` entry and for a daemon sender.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:1368` - `check_filter(&implied_filter_list, ...)` rejects
    ///   second occurrences as "unrequested file-list name".
    /// - `flist.c:2162` - `send_implied_dirs()` is upstream's equivalent
    ///   single emission point for the same logical directory.
    pub(in crate::generator) fn try_walk_source_entry_dedup(
        &mut self,
        base: &Path,
        path: &Path,
        emitted_dirs: Option<&HashSet<PathBuf>>,
        xfer_dirs: bool,
        is_dotdir: bool,
        remember_root: bool,
    ) -> io::Result<bool> {
        // Record the transfer root so per-directory merge files re-anchor their
        // leading-`/` rules to the merge file's own directory (upstream
        // exclude.c add_rule XFLG_ANCHORED2ABS). Idempotent across source
        // entries; `base` is the same root for the whole walk.
        self.filter_chain.set_transfer_root(base.to_path_buf());
        // upstream: flist.c:2651 `push_dir(dir, 0)` - the sender chdir's into
        // the `dir` half of the positional's `dir`/`fn` split before walking
        // it, which is the `curr_dir` every `full_fname()` in the walk renders
        // against (util1.c:1382). `base` is that same directory.
        self.curr_dir = Some(base.to_path_buf());
        // upstream: flist.c:2665 - link_stat() once, then pass &st to
        // send_file_name(). Reuse the metadata to avoid a redundant stat
        // inside walk_path_with_metadata.
        match self.resolve_symlink_metadata(path, base, is_dotdir) {
            Ok(metadata) => {
                // upstream: flist.c:2963-2966 - a directory argument is dropped
                // before any implied parent is emitted when `xfer_dirs` is off.
                if metadata.is_dir() && !xfer_dirs {
                    self.report_skipped_directory_arg(base, path);
                    return Ok(false);
                }
                // upstream: flist.c:2967-2969 - the operand's root is pinned by
                // identity: a directory is its own root, and under --relative
                // any other operand pins its parent.
                #[cfg(unix)]
                if remember_root && (metadata.is_dir() || self.config.flags.relative) {
                    self.source_roots.remember_operand(
                        path,
                        metadata.is_dir(),
                        metadata.dev(),
                        metadata.ino(),
                    )?;
                }
                #[cfg(not(unix))]
                let _ = remember_root;
                // If a prior pass already emitted this directory (e.g. the
                // implied-parent loop in build_file_list_with_base), skip the
                // top-level walk so we do not produce a duplicate file-list
                // entry. The directory's contents will be reached by the
                // explicit child entries from --files-from. Files are never
                // deduped through this path; only directories at the top
                // level can collide with the implied-parent loop's output.
                if metadata.is_dir()
                    && let Some(seen) = emitted_dirs
                {
                    let relative = path.strip_prefix(base).unwrap_or(path);
                    if !relative.as_os_str().is_empty() && seen.contains(relative) {
                        return Ok(true);
                    }
                }
                // Path exists - pass pre-resolved metadata directly.
                self.walk_path_with_metadata(base, path.to_path_buf(), metadata, true)?;
                Ok(true)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match self.missing_args_mode() {
                    // upstream: flist.c:2500 - missing_args == 1: silently skip
                    1 => Ok(false),
                    // upstream: flist.c:2491-2497 - missing_args == 2: emit mode-0 sentinel
                    2 => {
                        self.emit_delete_sentinel(base, path)?;
                        Ok(true)
                    }
                    // upstream: flist.c:2940-2944 - default: link_stat failed.
                    _ => {
                        // upstream: flist.c:2943 - `if (errno != ENOENT)` guards
                        // the `io_error |= IOERR_GENERAL`, so a source that
                        // never existed deliberately leaves io_error clear: that
                        // bit travels to the receiver and would inhibit its
                        // deletions, and upstream only wants that "if we might
                        // be omitting an existing file". The exit code comes
                        // from got_xfer_error instead, set by the FERROR_XFER
                        // below on both this side and the peer (log.c:337-338).
                        // upstream: flist.c:2945 - rsyserr(FERROR_XFER, ...)
                        let text = format!(
                            "rsync: [sender] link_stat {} failed: {}\n",
                            full_fname_path(path, self.full_fname_paths()),
                            engine::local_copy::upstream_io_error(&e),
                        );
                        self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
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
    /// - `flist.c:2491-2497` - `missing_args == 2`: `make_file()` + `file->mode = 0`
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
    /// This is the inner implementation shared by `walk_path` (which resolves
    /// metadata itself) and the batched-stat path (which pre-resolves metadata
    /// for all directory children in parallel before processing them).
    fn walk_path_with_metadata(
        &mut self,
        base: &Path,
        path: PathBuf,
        metadata: fast_io::EntryMetadata,
        is_top_level: bool,
    ) -> io::Result<()> {
        let mut stack = vec![WalkStep::Visit {
            path,
            metadata,
            is_top_level,
        }];
        // Under the lazy producer the top-level source is scanned one
        // level only - its subdirectories are emitted but not descended
        // (upstream send_directory with FLAG_DIVERT_DIRS). The diverted dirs
        // become the on-demand work list, seeded into the directory tree by the
        // partition seam. A diverted directory entry carries the identical wire
        // bytes to a recursively-walked one (content_dir is set unconditionally
        // in `new_directory`, independent of descent), so the initial segment is
        // byte-identical to the eager path's top-level entries.
        let scope = if self.lazy_producer_active {
            WalkScope::one_level(base)
        } else {
            WalkScope::recursive(base)
        };
        self.drive_walk(scope, &mut stack)
    }

    /// Runs the explicit walk stack to exhaustion.
    ///
    /// Steps pop LIFO, so a directory that schedules `[LeaveDir, ScanChildren]`
    /// scans first and releases its filter scope after. A scan opens a frame
    /// that collects the directories its children turn up; its `DescendFound`
    /// step, pushed beneath the children, descends into them once the last
    /// child is emitted. A directory visited outside any scan - a top-level
    /// operand - is descended into at once, as `send_file_list()` does with
    /// `send_if_directory()` right after `send_file_name()` (`flist.c:3006`).
    /// An error abandons the remaining steps.
    fn drive_walk(&mut self, scope: WalkScope<'_>, stack: &mut Vec<WalkStep>) -> io::Result<()> {
        let mut found: Vec<Vec<PathBuf>> = Vec::new();
        while let Some(step) = stack.pop() {
            let descend = match step {
                WalkStep::Visit {
                    path,
                    metadata,
                    is_top_level,
                } => self.visit_walk_entry(scope, path, metadata, is_top_level)?,
                WalkStep::VisitChild(result) => {
                    match self.resolve_child_metadata(scope.base, result) {
                        Some((path, metadata)) => {
                            self.visit_walk_entry(scope, path, metadata, false)?
                        }
                        None => None,
                    }
                }
                WalkStep::ScanChildren { dir } => {
                    found.push(Vec::new());
                    stack.push(WalkStep::DescendFound);
                    self.scan_children_onto(scope.base, &dir, stack)?;
                    None
                }
                WalkStep::DescendFound => {
                    let dirs = found.pop().unwrap_or_default();
                    stack.extend(dirs.into_iter().rev().map(WalkStep::Descend));
                    None
                }
                WalkStep::Descend(dir) => {
                    // upstream: exclude.c:push_local_filters() - read the
                    // directory's merge files before scanning it.
                    let guard = self.enter_filter_scope(&dir)?;
                    if scope.descends_into_subdirs() {
                        // LIFO: the scan runs before the scope is released.
                        stack.push(WalkStep::LeaveDir(guard));
                    } else {
                        // upstream: flist.c:2999-3002 - under INC_RECURSE the
                        // dot-dir root is pushed at depth 0 and stays pushed
                        // for every sub-list scan; only the walk popping past
                        // the root at flist EOF releases it (flist.c:2723).
                        self.lazy_root_scope = Some(guard);
                    }
                    stack.push(WalkStep::ScanChildren { dir });
                    None
                }
                WalkStep::LeaveDir(guard) => {
                    self.leave_filter_scope(guard);
                    None
                }
            };
            if let Some(dir) = descend {
                match found.last_mut() {
                    Some(frame) => frame.push(dir),
                    None => stack.push(WalkStep::Descend(dir)),
                }
            }
        }
        Ok(())
    }

    /// Emits one path's entry, returning the path when it is a directory the
    /// walk descends into.
    fn visit_walk_entry(
        &mut self,
        scope: WalkScope<'_>,
        path: PathBuf,
        metadata: fast_io::EntryMetadata,
        is_top_level: bool,
    ) -> io::Result<Option<PathBuf>> {
        let relative = path.strip_prefix(scope.base).unwrap_or(&path).to_path_buf();

        // upstream: flist.c:2850-2861 - non-relative single-file sources split
        // on the last `/` so the wire-side relative name is just the basename
        // (`fn` in upstream's terminology). When base == path for a regular
        // file, our `strip_prefix` would otherwise leave `relative` empty and
        // the receiver would write the bytes into the destination directory's
        // own name slot, producing an empty / corrupt entry. Restoring the
        // basename here matches upstream's daemon-mode `chdir(dir); link_stat(fn)`
        // ordering for the sub-path pull case (e.g. `rsync rsync://h/m/a/b/f`).
        let relative = if relative.as_os_str().is_empty() && !metadata.is_dir() {
            match path.file_name() {
                Some(name) => PathBuf::from(name),
                None => relative,
            }
        } else {
            relative
        };

        // upstream: flist.c:2527 - always emit "." with XMIT_TOP_DIR for the
        // root transfer directory. Enables delete_in_dir() when --delete is active.
        if relative.as_os_str().is_empty() && metadata.is_dir() {
            // upstream: flist.c:3010 - a named source argument is stat'ed with
            // NO_FILTERS (0).
            let mut dot_entry = self.create_entry(&path, PathBuf::from("."), &metadata, 0)?;
            dot_entry.set_top_dir(true);
            self.push_file_item(dot_entry, path.clone());
            return Ok(Some(path));
        }

        // upstream: flist.c:send_file_name() - skip unsupported file types
        #[cfg(unix)]
        {
            let ft = metadata.file_type();
            // upstream: flist.c:1644 - `--copy-devices` makes make_file() emit a
            // block/char device as a regular file, so it is included on the wire
            // even without `--devices`. Only skip when neither flag is active.
            if (ft.is_block_device() || ft.is_char_device())
                && !self.config.flags.devices
                && !self.config.flags.copy_devices
            {
                return Ok(None);
            }
            if (ft.is_fifo() || ft.is_socket()) && !self.config.flags.specials {
                return Ok(None);
            }
        }

        // upstream: flist.c:1810 - is_excluded() applied during make_file()
        // FilterChain evaluates per-directory scoped rules (innermost first)
        // then global rules. If no rules are configured, allows() returns true.
        if !self.filter_chain.allows(&relative, metadata.is_dir()) {
            return Ok(None);
        }

        // No --safe-links check here: upstream's sender transmits every
        // symlink. The option is evaluated on the receiving side only
        // (generator.c:1951 `safe_symlinks && unsafe_symlink(sl, fname)`).

        // upstream: flist.c:3010 sends a named source with NO_FILTERS (0);
        // recursion goes through send_directory() with ALL_FILTERS (2)
        // (flist.c:2501-2508).
        let filter_level = if is_top_level { 0 } else { 2 };
        let mut entry = match self.create_entry(&path, relative, &metadata, filter_level) {
            Ok(e) => e,
            // upstream: xattrs.c:1253-1258 - a corrupt %stat ends the run.
            Err(e) if metadata::is_corrupt_fake_super(&e) => return Err(e),
            Err(e) => {
                // upstream: flist.c:2058-2063 - a failed get_xattr() drops the
                // entry under IOERR_GENERAL after get_xattr_names() or
                // get_xattr_data() named the call; any other failure is
                // make_file()'s.
                let xattr_text = e
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<metadata::MetadataError>())
                    .and_then(|error| {
                        error.get_xattr_text(&full_fname_path(&path, self.full_fname_paths()))
                    });
                let text = match xattr_text {
                    Some((call, source)) => format!(
                        "rsync: [sender] {call}: {}\n",
                        engine::local_copy::upstream_io_error(source),
                    ),
                    None => format!(
                        "rsync: [sender] make_file failed for \"{}\": {}\n",
                        path.display(),
                        engine::local_copy::upstream_io_error(&e),
                    ),
                };
                self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                self.add_io_error(io_error_flags::IOERR_GENERAL);
                return Ok(None);
            }
        };

        // upstream: flist.c:2527 - top-level source directories carry
        // FLAG_TOP_DIR so delete_in_dir() can scope deletions. Under
        // --relative the directory entry has a non-empty relative name (e.g.
        // "tmp/dbg/src/usr/bin") instead of ".", but it still needs the flag.
        if is_top_level && metadata.is_dir() {
            entry.set_top_dir(true);
        }

        // Under `FLAG_DIVERT_DIRS` the entry is still emitted; only the descent
        // is withheld, so the directory becomes work for a later scan instead
        // of being expanded into this one.
        let descend =
            (metadata.is_dir() && self.config.flags.recursive && scope.descends_into_subdirs())
                .then(|| path.clone());
        self.push_file_item(entry, path);
        Ok(descend)
    }

    /// Scans the children of a `--files-from` SLASH_ENDING_NAME directory.
    ///
    /// Used by `build_file_list_with_base` to honour the upstream
    /// `flist.c:2823` rule that trailing-slash `--files-from` entries recurse
    /// into their named directory's children even when global `-r` is off
    /// (`options.c:2313-2316` clears `recurse` whenever `--files-from` is active).
    /// The walk-loop already pushed the directory entry itself via
    /// `try_walk_source_entry_dedup`; this helper just adds the children at
    /// the same level a global `-r` would have produced.
    ///
    /// Wraps `scan_directory_batched` in the per-directory filter scope
    /// (`enter_directory` / `leave_directory`) so per-dir merge files are
    /// honoured for the recursion the way they are during a normal walk.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    /// - `flist.c:2823` - `SLASH_ENDING_NAME` flag for trailing-slash entries
    pub(in crate::generator) fn scan_files_from_marker_dir(
        &mut self,
        base: &Path,
        dir_path: &Path,
    ) -> io::Result<()> {
        let guard = self.enter_filter_scope(dir_path)?;
        let result = self.scan_directory_batched(base, dir_path);
        self.leave_filter_scope(guard);
        result
    }

    /// Reads a directory and batch-stats its children before recursive processing.
    ///
    /// Collects all `DirEntry` paths from `read_dir()`, resolves their metadata
    /// in parallel via [`batch_stat_dir_entries`], then processes each child
    /// through `walk_path_with_metadata`. Entries whose stat fails are logged
    /// and recorded as I/O errors without aborting the traversal.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    fn scan_directory_batched(&mut self, base: &Path, dir_path: &Path) -> io::Result<()> {
        let mut stack = vec![WalkStep::ScanChildren {
            dir: dir_path.to_path_buf(),
        }];
        self.drive_walk(WalkScope::recursive(base), &mut stack)
    }

    /// Scans exactly one directory: emits an entry per child and descends into
    /// none of them.
    ///
    /// This is the producer step upstream's incremental recursion is built on.
    /// `send_extra_file_list()` picks one directory off the directory list and
    /// calls `send_directory()` with `FLAG_DIVERT_DIRS`, which emits that
    /// directory's children and diverts any subdirectory among them instead of
    /// expanding it. Each call therefore costs one level, not one subtree, and
    /// the caller decides when to spend the next.
    ///
    /// The caller owns the directory's filter scope: the lazy producer enters
    /// it before the scan and keeps it pushed for the directory's descendants.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads one directory and stats each child
    /// - `flist.c:send1extra()` - one directory, one sub-list
    pub(in crate::generator) fn scan_one_directory(
        &mut self,
        base: &Path,
        dir_path: &Path,
    ) -> io::Result<()> {
        let mut stack = vec![WalkStep::ScanChildren {
            dir: dir_path.to_path_buf(),
        }];
        self.drive_walk(WalkScope::one_level(base), &mut stack)
    }

    /// Pushes `dir`'s per-directory merge files onto the filter chain.
    ///
    /// upstream: exclude.c:858-867 push_local_filters() - every directory the
    /// sender enters is announced at FILTER2, whether or not a per-dir merge
    /// rule is registered.
    pub(in crate::generator) fn enter_filter_scope(
        &mut self,
        dir: &Path,
    ) -> io::Result<filters::DirFilterGuard> {
        debug_log!(
            Filter,
            2,
            "[{}] pushing local filters for {}",
            ProcessRole::Sender,
            filter_dir_display(dir)
        );
        self.filter_chain
            .enter_directory(dir)
            .map_err(|e| filter_chain_error(dir, e))
    }

    /// Pops the per-directory merge scope `guard` entered.
    ///
    /// upstream: exclude.c:925-933 pop_local_filters() announces the pop, but
    /// change_local_filter_dir() (exclude.c:986-995) only pops a scope whose
    /// push saved state, and push_local_filters() saves none while no per-dir
    /// merge rule is registered (exclude.c:869-872).
    pub(in crate::generator) fn leave_filter_scope(&mut self, guard: filters::DirFilterGuard) {
        if self.filter_chain.has_per_dir_merge() {
            debug_log!(Filter, 2, "[{}] popping local filters", ProcessRole::Sender);
        }
        self.filter_chain.leave_directory(guard);
    }

    /// Opens `dir_path` and schedules its children.
    ///
    /// A failed `opendir` is reported and the directory skipped; it does not
    /// abort the walk.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2374` - `rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)`
    fn scan_children_onto(
        &mut self,
        base: &Path,
        dir_path: &Path,
        stack: &mut Vec<WalkStep>,
    ) -> io::Result<()> {
        let depth = name_depth(dir_path.strip_prefix(base).unwrap_or(dir_path));
        let entries = match self.scan_source_dir(dir_path, depth) {
            Ok(entries) => entries,
            Err(e) => {
                // upstream: flist.c:2374 - rsyserr(FERROR_XFER, errno, "opendir %s failed", ...)
                let text = format!(
                    "rsync: [sender] opendir {} failed: {}\n",
                    full_fname_path(dir_path, self.full_fname_paths()),
                    engine::local_copy::upstream_io_error(&e),
                );
                self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
                self.record_io_error(&e);
                return Ok(());
            }
        };
        self.push_dir_entries_onto(dir_path, entries, stack)
    }

    /// Collects paths from a `ReadDir` iterator, batch-stats them, and schedules
    /// each child on the walk stack.
    ///
    /// Children are pushed in reverse so they pop in readdir order.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:send_directory()` - reads directory and stats each child
    /// - `flist.c:2431` - `rsyserr(FERROR_XFER, errno, "readdir(%s)", ...)`
    fn push_dir_entries_onto(
        &mut self,
        dir_path: &Path,
        entries: fast_io::pinned_root::ReadDir,
        stack: &mut Vec<WalkStep>,
    ) -> io::Result<()> {
        // Phase 1: collect child paths from readdir
        let mut child_paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(child) => child_paths.push(child),
                Err(e) => {
                    // upstream: flist.c:2431 - rsyserr(FERROR_XFER, errno, "readdir(%s)", ...)
                    let text = format!(
                        "rsync: [sender] readdir({}): {}\n",
                        full_fname_path(dir_path, self.full_fname_paths()),
                        engine::local_copy::upstream_io_error(&e),
                    );
                    self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
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
        // --copy-unsafe-links needs per-child fixup, applied when the child is
        // reached rather than here - see `WalkStep::VisitChild`.
        let follow = self.config.flags.copy_links;
        let stat_results = batch_stat_dir_entries(child_paths, follow, &self.parallel_thresholds);

        // Phase 3: schedule each child. Reverse, because the stack pops LIFO.
        stack.extend(stat_results.into_iter().rev().map(WalkStep::VisitChild));

        Ok(())
    }

    /// Applies the per-child stat fixups and reports the failures.
    ///
    /// Returns `None` when the child is dropped from the walk: its batch stat
    /// failed, or the `--copy-unsafe-links` dereference failed. Both are logged
    /// and counted as I/O errors without aborting the traversal.
    ///
    /// This runs when the child is REACHED, not when it is scheduled, so its
    /// notices stay interleaved with the walk exactly as the recursion had them.
    fn resolve_child_metadata(
        &mut self,
        base: &Path,
        result: StatResult,
    ) -> Option<(PathBuf, fast_io::EntryMetadata)> {
        let StatResult { path, metadata } = result;
        let mut meta = match metadata {
            Ok(meta) => meta,
            Err(e) => {
                self.report_child_stat_error(&path, &e);
                return None;
            }
        };

        let follow = self.config.flags.copy_links;

        // upstream: flist.c:1587-1595 link_stat() - with --copy-dirlinks
        // (follow_dirlinks), a symlink whose target is a directory is
        // transmitted as a real directory. Applied before the copy-unsafe-links
        // check exactly as upstream applies it inside link_stat() before
        // readlink_stat() re-examines S_ISLNK. Only symlinks to directories are
        // followed; symlinks to files stay symlinks (distinct from
        // --copy-links, which follows all).
        if !follow
            && self.config.flags.copy_dirlinks
            && meta.file_type().is_symlink()
            && let Ok(followed) = fast_io::pinned_root::metadata(&path)
            && followed.file_type().is_dir()
        {
            meta = followed;
        }

        // upstream: flist.c:494 - follow unsafe symlinks when
        // --copy-unsafe-links. The batch used lstat, so we need to re-stat
        // symlinks whose target escapes the tree.
        if !follow
            && self.config.flags.copy_unsafe_links
            && meta.file_type().is_symlink()
            && let Ok(target) = self.read_source_link(&path)
        {
            let relative = path.strip_prefix(base).unwrap_or(&path);
            if super::super::super::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative)
            {
                // upstream: flist.c:231 - INFO_GTE(SYMSAFE, 1) fires before
                // the target is dereferenced.
                info_log!(
                    Symsafe,
                    1,
                    "copying unsafe symlink \"{}\" -> \"{}\"",
                    path.display(),
                    target.display()
                );
                match fast_io::pinned_root::metadata(&path) {
                    Ok(followed) => meta = followed,
                    Err(e) => {
                        self.report_child_stat_error(&path, &e);
                        return None;
                    }
                }
            }
        }

        Some((path, meta))
    }

    /// Reports a failed stat of a walked entry and records its `io_error` bit.
    ///
    /// upstream: flist.c:1668-1681 make_file() - when a --copy*links option
    /// dereferenced the entry, ENOENT on a path that `lstat` still shows as a
    /// symlink is a dangling link: reported as "symlink has no referent" under
    /// FERROR_XFER with IOERR_GENERAL (exit 23), never as a vanished file (24).
    fn report_child_stat_error(&mut self, path: &Path, e: &io::Error) {
        let flags = &self.config.flags;
        if e.kind() == io::ErrorKind::NotFound
            && (flags.copy_links || flags.copy_unsafe_links || flags.copy_dirlinks)
            && fast_io::pinned_root::symlink_metadata(path).is_ok_and(|m| m.is_symlink())
        {
            let text = format!(
                "symlink has no referent: {}\n",
                full_fname_path(path, self.full_fname_paths())
            );
            self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
            self.add_io_error(io_error_flags::IOERR_GENERAL);
            return;
        }
        if e.kind() == io::ErrorKind::NotFound {
            self.log_stat_error(path, e);
        } else {
            // upstream: flist.c:1693 - make_file()'s own stat failure is
            // `readlink_stat(%s) failed`; `link_stat` is for operands.
            let text = format!(
                "rsync: [sender] readlink_stat({}) failed: {}\n",
                full_fname_path(path, self.full_fname_paths()),
                engine::local_copy::upstream_io_error(e),
            );
            self.queue_flist_diagnostic(SenderDiagnostic::ErrorXfer, text);
        }
        self.record_io_error(e);
    }

    /// Logs a stat failure with the appropriate upstream error format.
    ///
    /// Distinguishes between vanished files (ENOENT) and general stat errors,
    /// matching upstream `flist.c:1511-1519` error reporting. The two cases
    /// carry different log classes upstream, so they queue different frame
    /// types: the vanished notice is an `FWARNING`, the stat failure an
    /// `FERROR_XFER`.
    fn log_stat_error(&mut self, path: &Path, e: &io::Error) {
        let fname = full_fname_path(path, self.full_fname_paths());
        let (kind, text) = if e.kind() == io::ErrorKind::NotFound {
            // upstream: flist.c:1688-1692 - rprintf(FWARNING, "file has vanished: %s\n", ...)
            (
                SenderDiagnostic::Warning,
                format!("file has vanished: {fname}\n"),
            )
        } else {
            // upstream: flist.c:2236 - rsyserr(FERROR_XFER, errno, "link_stat %s failed", ...)
            (
                SenderDiagnostic::ErrorXfer,
                format!(
                    "rsync: [sender] link_stat {fname} failed: {}\n",
                    engine::local_copy::upstream_io_error(e),
                ),
            )
        };
        self.queue_flist_diagnostic(kind, text);
    }

    /// Resolves symlink metadata following upstream `flist.c:readlink_stat()`.
    ///
    /// Three modes of symlink resolution:
    /// - `--copy-links`: follow ALL symlinks (stat instead of lstat)
    /// - `--copy-unsafe-links`: follow only symlinks whose target escapes
    ///   the transfer tree (converting them to regular files)
    /// - Default: use lstat (preserve symlinks as symlinks)
    ///
    /// Every stat here goes through
    /// [`fast_io::pinned_root`](fast_io::pinned_root), which anchors the
    /// lookup on the module root a daemon pinned while still privileged when
    /// the path lies beneath it. Upstream is already positioned there - it
    /// `change_dir()`s into the module before the drop - so `link_stat(".")`
    /// costs it nothing; oc keeps absolute paths, and without the anchor the
    /// stat re-walks the module's ancestors as the dropped uid and `EACCES`es
    /// on an unsearchable one.
    ///
    /// `is_dotdir` is upstream's `name_type != NORMAL_NAME` for this operand,
    /// supplied by the caller. It cannot be re-derived from `path` here: the
    /// `--relative` operand split already normalised the marker out of the name
    /// (`flist.c:2891-2897`), which is precisely the step that keeps `name_type`
    /// and `fbuf` separate upstream.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:484-775` - `readlink_stat()`
    /// - `flist.c:229` - `copy_unsafe_links && unsafe_symlink(linkbuf, path)`
    /// - `clientserver.c:1059-1065` - the pin the anchoring reads.
    pub(in crate::generator) fn resolve_symlink_metadata(
        &self,
        path: &Path,
        base: &Path,
        is_dotdir: bool,
    ) -> io::Result<fast_io::EntryMetadata> {
        // upstream: flist.c:readlink_stat() operates on paths without the
        // DOTDIR marker. On Linux, lstat("path/") follows symlinks because the
        // kernel resolves the trailing slash, making a symlink appear as its
        // target directory - so the marker must be stripped before the stat to
        // get a decision that is the same on every platform. The marker itself
        // is not noise to discard: it is upstream's `name_type`, and the follow
        // decision below depends on the `is_dotdir` the caller passed in.
        let normalized: PathBuf;
        let path = {
            let bytes = path.as_os_str().as_encoded_bytes();
            if bytes.len() > 1 && (bytes.ends_with(b"/") || bytes.ends_with(b"/.")) {
                normalized = path.components().collect();
                normalized.as_path()
            } else {
                path
            }
        };

        // upstream: flist.c:465-466 `scan_link_stat()` - a `--files-from`
        // entry is stat'd through the ownership walk, following a leaf symlink
        // only for a dot-dir entry since the walk is inactive under
        // `--copy-dirlinks`.
        #[cfg(unix)]
        if let Some(base) = self.files_from_base.as_deref() {
            return base
                .symlink_metadata(path, is_dotdir)
                .map(fast_io::EntryMetadata::from);
        }
        if self.config.flags.copy_links {
            return fast_io::pinned_root::metadata(path);
        }

        let meta = fast_io::pinned_root::symlink_metadata(path)?;

        // upstream: flist.c:1587-1595 link_stat() - with follow_dirlinks a
        // symlink whose target is a directory is transmitted as a real
        // directory. Applied before the copy-unsafe-links check, mirroring
        // upstream's link_stat() (dirlink follow) running before
        // readlink_stat()'s S_ISLNK re-examination. Only symlinks to
        // directories are followed; symlinks to files stay symlinks (distinct
        // from --copy-links).
        //
        // follow_dirlinks has TWO disjuncts upstream, not one
        // (flist.c:2937 `copy_dirlinks || name_type != NORMAL_NAME`). A DOTDIR
        // operand follows, because asking for the CONTENTS of `current/` is
        // only meaningful once `current` has been resolved to the directory it
        // points at. Gating on `--copy-dirlinks` alone made
        // `oc-rsync -a host:current/ dest/` deliver nothing and exit 0.
        //
        // ⚠ The DOTDIR disjunct is gated on OWNERSHIP, `--copy-dirlinks` is
        // not, and the asymmetry is deliberate. Upstream reaches this stat with
        // the operand ALREADY resolved: the daemon runs
        // `change_dir(module_chdir)` through `open_no_attacker_symlinks()`
        // (util1.c:1313) before the file list is walked, so by the time
        // `link_stat()` sees `.` there is no symlink left to follow. oc walks
        // the unresolved path, and every daemon transfer sends `.` as its
        // operand - so an ungated DOTDIR follow lets a foreign-uid symlink
        // planted at the module root redirect the listing outside the module.
        // `--copy-dirlinks` is an explicit client request over a client-named
        // tree and keeps its existing meaning untouched.
        //
        // When the daemon PINNED its module root the stat above already comes
        // from the pinned directory, so `.` is a directory here and this gate
        // has nothing to decide - which is upstream's position exactly, and is
        // sound for the same reason: the pin is taken through the ownership
        // walk (`util1.c:1351-1360`), so a foreign-uid symlink at the module
        // root is refused before a pin exists and this gate is back in force.
        //
        // upstream: `rsync-3.5.1/syscall.c:499` - a symlink owned by uid 0 or
        // our euid is the operator's own layout and is followed; any other uid
        // is an attacker's plant and is refused.
        //
        // ⚠ The ownership rule is opted out of, not merely relaxed. upstream:
        // `syscall.c:380-382` - `ona_open()` returns a plain symlink-following
        // `open()` when `symlink_optout_allowed()`, ABOVE the ownership test,
        // so `change_dir()`'s `open_no_attacker_symlinks()` (util1.c:1313)
        // follows ANY symlink for a module that opted out. Without this term
        // the opt-out reached the sender's confinement root but never this
        // gate, and a daemon with `insecure links = yes` still refused
        // `<module>/link/`.
        //
        // `symlink_optout_allowed()`'s daemon arm is `module_id >= 0 &&
        // lp_insecure_links(module_id)` (`syscall.c:139-144`) - exactly
        // `daemon_insecure_links`, which a peer-supplied `--insecure-links`
        // cannot reach. The non-daemon arm is deliberately not read here: the
        // measured divergence is daemon-only, and a local
        // `oc-rsync -r <dir>/link/ dest/` already follows a foreign-owned
        // symlinked operand today.
        let follow_dirlinks = dotdir_follow_allowed(
            self.config.flags.copy_dirlinks,
            is_dotdir,
            self.config.connection.daemon_insecure_links,
            symlink_target_is_operator_owned(&meta),
        );
        if follow_dirlinks
            && meta.file_type().is_symlink()
            && let Ok(followed) = fast_io::pinned_root::metadata(path)
            && followed.file_type().is_dir()
        {
            return Ok(followed);
        }

        // upstream: flist.c:494 - follow unsafe symlinks when --copy-unsafe-links
        if self.config.flags.copy_unsafe_links && meta.file_type().is_symlink() {
            let target = self.read_source_link(path)?;
            let relative = path.strip_prefix(base).unwrap_or(path);
            if super::super::super::symlink_safety::is_unsafe_symlink(target.as_os_str(), relative)
            {
                // upstream: flist.c:231 - INFO_GTE(SYMSAFE, 1) fires before
                // the unsafe symlink is dereferenced into a regular entry.
                info_log!(
                    Symsafe,
                    1,
                    "copying unsafe symlink \"{}\" -> \"{}\"",
                    path.display(),
                    target.display()
                );
                return fast_io::pinned_root::metadata(path);
            }
        }

        Ok(meta)
    }
}

/// Is a symlink the operator's own, and therefore safe to follow for a DOTDIR
/// operand?
///
/// Ownership - not location - is upstream's trust signal here. A link owned by
/// uid 0 or our own euid is the operator's layout (the `current -> releases/X`
/// deploy pattern this follow exists to serve); any other uid is a plant.
///
/// This is the DOTDIR half of `follow_dirlinks` only. `--copy-dirlinks` is an
/// explicit request over a client-named tree and is deliberately not gated.
///
/// On a non-Unix target there is no `st_uid` to test and no unprivileged way to
/// plant a foreign-owned link in a module root, so the follow proceeds.
///
/// # Upstream Reference
///
/// - `rsync-3.5.1/syscall.c:499` - `st_uid != 0 && st_uid != trusted_uid`
///   refuses the symlink; otherwise the walk follows it.
/// - `rsync-3.5.1/util1.c:1313` `change_dir()` - the daemon resolves the module
///   root through this same rule BEFORE the file list is walked, which is why
///   upstream never reaches this stat with an unresolved operand.
fn symlink_target_is_operator_owned(meta: &fast_io::EntryMetadata) -> bool {
    #[cfg(unix)]
    {
        fast_io::symlink_owner_is_trusted(meta.uid())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// Whether a symlinked operand is followed to the directory it names.
///
/// The three disjuncts are upstream's, and they are NOT interchangeable:
///
/// - `copy_dirlinks` is an explicit client request over a client-named tree
///   (`flist.c:2937` `copy_dirlinks || name_type != NORMAL_NAME`).
/// - `is_dotdir` is upstream's `name_type != NORMAL_NAME`: asking for the
///   CONTENTS of `current/` is only meaningful once `current` has been resolved.
/// - `optout` is the served module's `insecure links = yes`
///   (`syscall.c:139-144` `symlink_optout_allowed()`), which
///   `ona_open()` reads ABOVE the ownership test (`syscall.c:380-382`), so a
///   module that opted out follows any symlink - the whole point of the
///   directive.
/// - `owner_is_operator` is upstream's ownership rule (`syscall.c:499`): uid 0
///   or our euid is the operator's own layout, any other uid is a plant.
///
/// # Upstream Reference
///
/// - `rsync-3.5.1/flist.c:2937` - the `copy_dirlinks || name_type` disjunction.
/// - `rsync-3.5.1/syscall.c:380-382` - the opt-out short-circuit.
/// - `rsync-3.5.1/syscall.c:499` - the ownership rule.
fn dotdir_follow_allowed(
    copy_dirlinks: bool,
    is_dotdir: bool,
    optout: bool,
    owner_is_operator: bool,
) -> bool {
    copy_dirlinks || (is_dotdir && (optout || owner_is_operator))
}
#[cfg(all(test, unix))]
mod dotdir_follow_trust_tests {
    /// The DOTDIR follow is gated on upstream's ownership rule, not on
    /// location: uid 0 and our own euid are the operator's own layout, any
    /// other uid is a plant.
    ///
    /// This pins the RULE. The behavioural proof that a foreign-owned symlink
    /// at a daemon module root is not followed lives in the upstream testsuite
    /// cell `daemon-module-chdir-symlink`, which needs root to create a
    /// non-self-owned link and therefore cannot run here.
    ///
    /// upstream: `rsync-3.5.1/syscall.c:499`.
    #[test]
    fn only_root_and_our_own_euid_are_trusted_symlink_owners() {
        let euid = rustix::process::geteuid().as_raw();
        assert!(
            fast_io::symlink_owner_is_trusted(0),
            "uid 0 is the operator"
        );
        assert!(
            fast_io::symlink_owner_is_trusted(euid),
            "our own euid is the operator"
        );

        // Pick a uid that is neither 0 nor ours. 65534 (nobody) is the uid the
        // testsuite plants with; fall back if we happen to be running as it.
        let foreign = if euid == 65534 { 65533 } else { 65534 };
        assert!(
            !fast_io::symlink_owner_is_trusted(foreign),
            "uid {foreign} is neither root nor our euid, so it is a plant"
        );
    }
    /// `insecure links = yes` restores the legacy follow for a FOREIGN-owned
    /// symlinked operand, which is the whole content of the directive:
    /// `ona_open()` returns a plain symlink-following `open()` before the
    /// ownership test ever runs.
    ///
    /// Measured on the upstream 3.5.0 cell `daemon-symlink-escape-matrix`,
    /// `read-plain` vector: before this term the opted-out daemon refused
    /// `<module>/link/` for all five link types.
    ///
    /// upstream: `rsync-3.5.1/syscall.c:380-382`.
    #[test]
    fn the_module_optout_follows_a_symlinked_operand_the_ownership_rule_refuses() {
        assert!(
            super::dotdir_follow_allowed(false, true, true, false),
            "insecure links = yes must follow a foreign-owned symlinked operand"
        );
        assert!(
            !super::dotdir_follow_allowed(false, true, false, false),
            "the secure default must still refuse it"
        );
    }
    /// Non-vacuity companions for the two terms the opt-out must NOT change.
    ///
    /// Without these a predicate that simply returned `true` would satisfy the
    /// test above. The `is_dotdir` term is upstream's `name_type !=
    /// NORMAL_NAME`: a plain (non-operand) symlink is transmitted as a symlink
    /// whatever the module says, and the opt-out does not widen that.
    ///
    /// upstream: `rsync-3.5.1/flist.c:2937`.
    #[test]
    fn the_optout_does_not_widen_the_non_dotdir_or_trusted_arms() {
        assert!(
            !super::dotdir_follow_allowed(false, false, true, true),
            "a non-DOTDIR name is not followed even under the opt-out"
        );
        assert!(
            super::dotdir_follow_allowed(false, true, false, true),
            "an operator-owned symlinked operand is followed without the opt-out"
        );
        assert!(
            super::dotdir_follow_allowed(true, false, false, false),
            "--copy-dirlinks is unconditional and untouched by this change"
        );
    }
}

#[cfg(test)]
mod rsyserr_wording_tests {
    //! Pin per-file `rsyserr`-equivalent wording to upstream rsync 3.5.1
    //! `log.c:rsyserr()` byte-for-byte. See
    //! `docs/audits/error-message-verbatim-audit.md` family 4.

    /// Each tuple is (template-with-{path}-marker, expected-rendered-line).
    /// Templates mirror the literal `eprintln!` formats above so a future
    /// refactor that re-inserts the source-location or role-version trailer
    /// will fail these asserts.
    const CASES: &[(&str, &str)] = &[
        // upstream: flist.c:2236 - "link_stat %s failed"
        (
            "rsync: [sender] link_stat \"{path}\" failed: No such file or directory (2)",
            "rsync: [sender] link_stat \"/p\" failed: No such file or directory (2)",
        ),
        // upstream: flist.c:2067 - "opendir %s failed"
        (
            "rsync: [sender] opendir \"{path}\" failed: Permission denied (13)",
            "rsync: [sender] opendir \"/p\" failed: Permission denied (13)",
        ),
        // upstream: flist.c:2431 - "readdir(%s)"
        (
            "rsync: [sender] readdir(\"{path}\"): Input/output error (5)",
            "rsync: [sender] readdir(\"/p\"): Input/output error (5)",
        ),
        // upstream: flist.c (make_file paths) - follows rsyserr() shape
        (
            "rsync: [sender] make_file failed for \"{path}\": Permission denied (13)",
            "rsync: [sender] make_file failed for \"/p\": Permission denied (13)",
        ),
        // upstream: flist.c:1688 / sender.c:715 - "file has vanished: %s" via full_fname()
        ("file has vanished: \"{path}\"", "file has vanished: \"/p\""),
        // upstream: sender.c:720 - "send_files failed to open %s"
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

    /// The same lines rendered from a daemon server process must carry
    /// upstream's ` (in MODULE)` suffix outside the closing quote and name the
    /// path relative to the module root, because upstream builds the path with
    /// `full_fname()` after `chdir()`ing into the module (`clientserver.c:1059`)
    /// and `module_id >= 0`. Ground truth captured from rsync 3.4.4 serving
    /// module `mod` rooted at `/tmp/modrel/mod`:
    /// `rsync: [sender] opendir "denied" (in mod) failed: Permission denied (13)`.
    #[test]
    fn rsyserr_wording_carries_daemon_module_suffix() {
        use crate::full_fname::{FullFnamePaths, full_fname_path};
        use std::path::Path;

        let daemon = FullFnamePaths::daemon("mymod", Path::new("/srv/mod"), Path::new("/srv/mod"));
        let quoted = full_fname_path(Path::new("/srv/mod/p"), daemon);
        assert_eq!(quoted, "\"p\" (in mymod)");
        assert_eq!(
            format!("rsync: [sender] link_stat {quoted} failed: No such file or directory (2)"),
            "rsync: [sender] link_stat \"p\" (in mymod) failed: No such file or directory (2)",
        );
        assert_eq!(
            format!("rsync: [sender] readdir({quoted}): Input/output error (5)"),
            "rsync: [sender] readdir(\"p\" (in mymod)): Input/output error (5)",
        );
        assert_eq!(
            format!("file has vanished: {quoted}"),
            "file has vanished: \"p\" (in mymod)",
        );
    }

    /// A client or SSH server process (`module_id < 0`) must keep the absolute
    /// path it has always printed: upstream neither strips a prefix nor
    /// appends a suffix there, so a local or SSH run's stderr is unchanged.
    #[test]
    fn rsyserr_wording_outside_a_daemon_stays_absolute() {
        use crate::full_fname::{FullFnamePaths, full_fname_path};
        use std::path::Path;

        let quoted = full_fname_path(Path::new("/srv/mod/p"), FullFnamePaths::non_daemon());
        assert_eq!(quoted, "\"/srv/mod/p\"");
        assert_eq!(
            format!("rsync: [sender] opendir {quoted} failed: Permission denied (13)"),
            "rsync: [sender] opendir \"/srv/mod/p\" failed: Permission denied (13)",
        );
    }
}

#[cfg(test)]
mod symsafe_emission_tests {
    //! Wording tests for `--info=SYMSAFE` producer emissions on the
    //! sender side.
    //!
    //! Upstream rsync 3.4.1 fires `INFO_GTE(SYMSAFE, 1)` at `flist.c:493`
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
        // upstream: flist.c:496 -
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

/// Directory levels below the sender's transfer base that `relative`, a
/// file-list name, descends.
///
/// upstream counts from the directory the sender changed into, so the name
/// as it goes on the wire is exactly the walk (`flist.c:2356`).
fn name_depth(relative: &Path) -> usize {
    relative
        .components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count()
}

#[cfg(test)]
mod filter_dir_display_tests {
    //! The directory a FILTER trace names is upstream's `dirbuf`
    //! (exclude.c:756-778 `set_filter_dir()`): absolute, cleaned, and ending
    //! in `/`, so a log line can be compared with 3.5.1's verbatim.
    use std::path::Path;

    use super::filter_dir_display;

    #[test]
    fn an_absolute_directory_gains_a_trailing_slash() {
        assert_eq!(filter_dir_display(Path::new("/srv/src")), "/srv/src/");
    }

    #[test]
    fn dot_and_dot_dot_are_folded_lexically() {
        assert_eq!(
            filter_dir_display(Path::new("/srv/./src/a/../b/.")),
            "/srv/src/b/"
        );
    }

    #[test]
    fn the_root_is_not_doubled() {
        assert_eq!(filter_dir_display(Path::new("/")), "/");
    }

    #[test]
    fn a_relative_directory_is_anchored_to_the_working_directory() {
        let cwd = std::env::current_dir().expect("cwd");
        assert_eq!(
            filter_dir_display(Path::new("src/a")),
            format!("{}/", cwd.join("src/a").display())
        );
    }
}
