//! Handling of reference directories and link-dest decisions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ::metadata::{MetadataOptions, ModifyWindow};

use crate::local_copy::{CopyContext, LocalCopyError, ReferenceDirectoryKind};

use super::{CopyComparison, should_skip_copy};

/// Outcome of evaluating a reference directory candidate against source metadata.
pub(crate) enum ReferenceDecision {
    /// A `--compare-dest` match: the file is skipped and itemized as `.f`
    /// against the carried basis path (blank columns when identical).
    Skip(PathBuf),
    Copy(PathBuf),
    Link(PathBuf),
}

/// Computes the full path for a reference directory candidate.
///
/// Absolute bases are joined directly; relative bases are resolved from
/// the destination ancestor at the same depth as `relative`. The result is
/// lexically normalized (collapsing `..`/`.`) so the candidate resolves even
/// when an intermediate directory (e.g. a not-yet-created dry-run destination)
/// does not exist on disk.
pub(crate) fn resolve_reference_candidate(
    base: &Path,
    relative: &Path,
    destination: &Path,
) -> PathBuf {
    if base.is_absolute() {
        base.join(relative)
    } else {
        let mut ancestor = destination.to_path_buf();
        let depth = relative.components().count();
        for _ in 0..depth {
            if !ancestor.pop() {
                break;
            }
        }
        crate::local_copy::lexically_normalize(&ancestor.join(base).join(relative))
    }
}

/// `lstat`s an alt-dest basis candidate with its parent resolved by the
/// ownership walk, mirroring upstream's `basis_link_stat()`.
///
/// A `--link-dest` / `--copy-dest` / `--compare-dest` argument is an
/// operator-supplied path, so it may legitimately point outside the transfer
/// tree and location cannot be the trust signal. Authority is: a symlink
/// component owned by uid 0 or our euid is the operator's own layout and is
/// followed; any other owner is refused. Without that, an unprivileged user who
/// controls a directory anywhere along the basis path can swap a component for a
/// symlink and redirect the stat - and then the hard link, the copy, or the
/// `--compare-dest` skip - at a file of their choosing.
///
/// A refusal is reported as "no basis here", which is upstream's outcome too:
/// `basis_link_stat()` returns `-1` and every call site turns that into a
/// `continue`, so the file transfers normally instead of being linked, copied or
/// skipped through an attacker's symlink.
///
/// Local copy has no daemon arm. Upstream selects the plain `link_stat()`
/// fall-through on `am_daemon` (generator.c:1010), whose confinement comes from
/// the module resolver instead; a local copy is never a daemon worker, so the
/// ownership walk is the only reachable arm here.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/generator.c:962` `basis_link_stat()` - the non-daemon arm:
///   `owner_walk_parent()` then `link_stat_at()` on the leaf.
/// - `rsync-3.5.0/generator.c:1084` / `:1110` / `:1227` / `:1254` - its call
///   sites in `try_dests_reg()` and `try_dests_non()`.
#[cfg(unix)]
fn basis_stat(path: &Path) -> io::Result<fs::Metadata> {
    fast_io::operator_symlink_metadata(path)
}

/// Non-Unix fallback: the ownership walk is a dirfd construction with no Windows
/// analogue, so the basis is stat'd by path. `symlink_metadata` still refuses to
/// follow a symlink at the leaf, which keeps a symlinked basis entry from being
/// consumed; only the parent components are unguarded.
#[cfg(not(unix))]
fn basis_stat(path: &Path) -> io::Result<fs::Metadata> {
    fs::symlink_metadata(path)
}

/// Parameters for finding a matching file in reference directories.
pub(crate) struct ReferenceQuery<'a> {
    pub(crate) destination: &'a Path,
    pub(crate) relative: &'a Path,
    pub(crate) source: &'a Path,
    pub(crate) metadata: &'a fs::Metadata,
    pub(crate) size_only: bool,
    pub(crate) ignore_times: bool,
    pub(crate) checksum: bool,
    /// Preserved-attribute options used to distinguish an exact (match_level 3)
    /// basis from a data-only (match_level 2) one.
    pub(crate) metadata_options: &'a MetadataOptions,
    /// Whether `-X` xattr preservation is active, so an xattr difference demotes
    /// a candidate from match_level 3 to match_level 2.
    pub(crate) preserve_xattrs: bool,
}

/// Splits a `SystemTime` into the `(seconds, nanoseconds)` pair `same_time`
/// compares, mirroring the `st_mtime` / `ST_MTIME_NSEC` split upstream reads
/// from `struct stat`. `fs::Metadata::modified` is the only portable source, so
/// the split happens here rather than through the Unix-only `MetadataExt`.
///
/// A pre-epoch time carries a negative second count with a positive nanosecond
/// remainder, exactly as `struct timespec` stores it.
fn unix_time_parts(time: SystemTime) -> (i64, u32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(since) => (
            i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            since.subsec_nanos(),
        ),
        Err(before) => {
            let delta = before.duration();
            let secs = i64::try_from(delta.as_secs()).unwrap_or(i64::MAX);
            match delta.subsec_nanos() {
                0 => (-secs, 0),
                nanos => (-secs - 1, 1_000_000_000 - nanos),
            }
        }
    }
}

/// Whether a reference basis carries the source's modification time under
/// `--modify-window`.
///
/// upstream: `rsync-3.5.0/util1.c:1649` `same_time()` - a zero window (the
/// default) compares whole seconds, a negative window compares seconds and
/// nanoseconds, and a positive window is a whole-second tolerance.
fn reference_mtime_matches(
    source_meta: &fs::Metadata,
    basis_meta: &fs::Metadata,
    modify_window: ModifyWindow,
) -> bool {
    let (Ok(source_time), Ok(basis_time)) = (source_meta.modified(), basis_meta.modified()) else {
        return false;
    };
    let (source_secs, source_nanos) = unix_time_parts(source_time);
    let (basis_secs, basis_nanos) = unix_time_parts(basis_time);
    modify_window.same_time(source_secs, source_nanos, basis_secs, basis_nanos)
}

/// Reports whether a reference basis already carries the source's preserved
/// attributes, mirroring upstream `generator.c:475 unchanged_attrs()`.
///
/// A `true` result is upstream match_level 3 (data and attributes both match):
/// the basis is hard-linked (`--link-dest`) or treated as up-to-date
/// (`--compare-dest`) with no attribute reapply, so no `user.rsync.%stat` xattr
/// is written onto a shared basis inode. A `false` result is match_level 2 (data
/// matches, attrs differ): upstream falls through to `copy_altdest_file`, copying
/// the basis into a fresh inode and reapplying the source attributes via
/// `set_file_attrs`.
///
/// Compares the attributes `unchanged_attrs` inspects for a regular file:
/// permission bits (`perms_differ`), owner/group (`ownership_differs`), mtime
/// (`any_time_differs`), and, when `-X` is active, the transferable extended
/// attributes (`xattrs_differ`), each gated on the corresponding preserve option.
/// upstream: generator.c:475-509 unchanged_attrs - perms/ownership/time/xattr.
pub(crate) fn reference_attrs_unchanged(
    basis: &Path,
    source: &Path,
    source_meta: &fs::Metadata,
    options: &MetadataOptions,
    modify_window: ModifyWindow,
    preserve_xattrs: bool,
) -> bool {
    let Ok(basis_meta) = basis_stat(basis) else {
        return false;
    };

    // A --chmod tweak changes the intended mode away from the source's, so
    // the basis (which carries the untweaked mode) can never be a
    // match_level-3 attrs match; force a reapply.
    if options.chmod().is_some() {
        return false;
    }

    // upstream: generator.c:400 mtime_differs -> util1.c:1649 same_time - the
    // mtime must match for a level-3 basis, and `same_time` compares WHOLE
    // SECONDS for the default `modify_window` of 0. Nanoseconds only enter the
    // comparison for a negative window (`--modify-window` < 0, nsec-exact), and
    // a positive window is a whole-second tolerance. Comparing `SystemTime`
    // directly would be nsec-exact always, so a basis whose mtime was not
    // preserved to nanosecond precision - the common case for anything not
    // written by rsync itself - is demoted from match_level 3 to 2 and copied
    // instead of hard-linked.
    if options.times() && !reference_mtime_matches(source_meta, &basis_meta, modify_window) {
        return false;
    }

    // upstream: generator.c:494 perms_differ - Unix compares the full permission
    // bits; on platforms without POSIX modes (Windows) the only preserved
    // permission is the read-only attribute, matching how oc applies and
    // quick-checks permissions there.
    if options.permissions() && !reference_permissions_match(source_meta, &basis_meta) {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        if options.owner() && source_meta.uid() != basis_meta.uid() {
            return false;
        }
        if options.group() && source_meta.gid() != basis_meta.gid() {
            return false;
        }
        // upstream: generator.c:501 - xattrs_differ() demotes to match_level 2,
        // forcing the copy + set_file_attrs that reapplies the source xattrs.
        // Owner, group, and xattrs have no meaningful equivalent on non-Unix
        // platforms, where oc preserves none of them.
        if preserve_xattrs && !::metadata::xattrs_match(source, basis, true).unwrap_or(false) {
            return false;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (source, preserve_xattrs);
    }

    true
}

/// Reports whether two files carry the same preserved permission bits.
///
/// On Unix this is the low 12 mode bits (`0o7777`); on other platforms it is the
/// read-only attribute, the only permission bit oc preserves there.
#[cfg(unix)]
fn reference_permissions_match(source_meta: &fs::Metadata, basis_meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    (source_meta.mode() & 0o7777) == (basis_meta.mode() & 0o7777)
}

#[cfg(not(unix))]
fn reference_permissions_match(source_meta: &fs::Metadata, basis_meta: &fs::Metadata) -> bool {
    source_meta.permissions().readonly() == basis_meta.permissions().readonly()
}

/// Searches configured reference directories for a file matching the source and
/// returns the action for the BEST candidate.
///
/// upstream: generator.c:954-1054 `try_dests_reg()` scans every `basis_dir[]`,
/// tracking the highest match_level (2 = data matches `quick_check_ok`, 3 = data
/// and attributes both match `unchanged_attrs`), breaking early only on an exact
/// (level-3) match, then acts on the best candidate. A first-match scan wrongly
/// picks an earlier data-only basis over a later exact one, forcing an
/// unnecessary copy/transfer and (for `--link-dest`) reapplying attrs onto a
/// shared basis inode. The winning level then drives the action
/// (generator.c:1007-1066):
///
/// - `--compare-dest`: level 3 is up-to-date (skip, no write); level 2 copies the
///   basis in and reapplies attrs (`copy_altdest_file`).
/// - `--copy-dest`: always copies the basis (never hard-links), reapplying attrs.
/// - `--link-dest`: level 3 hard-links without reapply; level 2 copies + reapplies
///   (upstream `try_a_copy`) so a differing-attr basis inode is never shared.
///
/// Returns `None` when no candidate reaches match_level 2 (a level-1 basis is
/// left to the normal transfer path).
pub(crate) fn find_reference_action(
    context: &CopyContext<'_>,
    query: ReferenceQuery<'_>,
) -> Result<Option<ReferenceDecision>, LocalCopyError> {
    let ReferenceQuery {
        destination,
        relative,
        source,
        metadata,
        size_only,
        ignore_times,
        checksum,
        metadata_options,
        preserve_xattrs,
    } = query;

    let mut best: Option<(ReferenceDirectoryKind, PathBuf, u8)> = None;
    for reference in context.reference_directories() {
        let candidate = resolve_reference_candidate(reference.path(), relative, destination);
        // upstream: generator.c:1084 `if (basis_link_stat(cmpbuf, &sxp->st) < 0
        // || !S_ISREG(sxp->st.st_mode)) continue;` - and likewise at :1110,
        // :1227 and :1254. EVERY caller treats ANY stat failure as "no candidate
        // in this basis dir", not just ENOENT. An alt-dest arg that is missing
        // or is not a directory has already been reported once by
        // check_alt_basis_dirs() (main.c:867); aborting the transfer on the
        // resulting ENOTDIR would fail a run upstream completes normally.
        let Ok(candidate_metadata) = basis_stat(&candidate) else {
            continue;
        };

        if !candidate_metadata.file_type().is_file() {
            continue;
        }

        if !should_skip_copy(CopyComparison {
            source_path: source,
            source: metadata,
            destination_path: &candidate,
            destination: &candidate_metadata,
            size_only,
            ignore_times,
            checksum,
            checksum_algorithm: context.options().checksum_algorithm(),
            modify_window: context.options().modify_window(),
            prefetched_match: None,
        }) {
            continue;
        }

        // The candidate is at least match_level 2 (data matches); it is
        // match_level 3 when its preserved attributes also match the source.
        let level = if reference_attrs_unchanged(
            &candidate,
            source,
            metadata,
            metadata_options,
            context.options().modify_window(),
            preserve_xattrs,
        ) {
            3
        } else {
            2
        };

        if best
            .as_ref()
            .is_none_or(|(_, _, best_level)| level > *best_level)
        {
            best = Some((reference.kind(), candidate, level));
        }
        // upstream: generator.c:979 - an exact match ends the scan immediately.
        if level == 3 {
            break;
        }
    }

    let Some((kind, basis, level)) = best else {
        return Ok(None);
    };

    let decision = match kind {
        ReferenceDirectoryKind::Compare => {
            if level == 3 {
                ReferenceDecision::Skip(basis)
            } else {
                ReferenceDecision::Copy(basis)
            }
        }
        ReferenceDirectoryKind::Copy => ReferenceDecision::Copy(basis),
        ReferenceDirectoryKind::Link => {
            if level == 3 {
                ReferenceDecision::Link(basis)
            } else {
                ReferenceDecision::Copy(basis)
            }
        }
    };

    Ok(Some(decision))
}

/// Locates a `--copy-dest` basis symlink at `relative` whose target matches.
///
/// Returns the basis symlink metadata when a `Copy` reference holds a symlink
/// pointing at `target`. A copy-dest match reconstructs the link from the basis
/// and itemizes it as a local change (`cL`) instead of a new entry.
///
/// upstream: generator.c:1094 quick_check_ok(FT_SYMLINK) compares link targets.
pub(crate) fn find_copy_dest_symlink(
    context: &CopyContext<'_>,
    destination: &Path,
    relative: &Path,
    target: &Path,
) -> Result<Option<fs::Metadata>, LocalCopyError> {
    find_reference_symlink(context, destination, relative, target, |kind| {
        kind == ReferenceDirectoryKind::Copy
    })
}

/// Locates a `--compare-dest` basis symlink at `relative` whose target matches.
///
/// A compare-dest match means the symlink already exists elsewhere, so the
/// receiver neither recreates it nor reports a transfer; it itemizes `.L`
/// against the basis.
///
/// upstream: generator.c:1140 - COMPARE_DEST forces `chg = 0` for non-directory
/// matches, so the update char stays `.`.
pub(crate) fn find_compare_dest_symlink(
    context: &CopyContext<'_>,
    destination: &Path,
    relative: &Path,
    target: &Path,
) -> Result<Option<fs::Metadata>, LocalCopyError> {
    find_reference_symlink(context, destination, relative, target, |kind| {
        kind == ReferenceDirectoryKind::Compare
    })
}

/// Shared symlink lookup across reference directories whose kind passes `accept`.
fn find_reference_symlink(
    context: &CopyContext<'_>,
    destination: &Path,
    relative: &Path,
    target: &Path,
    accept: impl Fn(ReferenceDirectoryKind) -> bool,
) -> Result<Option<fs::Metadata>, LocalCopyError> {
    if relative.as_os_str().is_empty() {
        return Ok(None);
    }
    for reference in context.reference_directories() {
        if !accept(reference.kind()) {
            continue;
        }
        let candidate = resolve_reference_candidate(reference.path(), relative, destination);
        // upstream: generator.c:1227 `if (basis_link_stat(cmpbuf, &sxp->st) < 0)
        // continue;` in try_dests_non() - ANY stat failure means "no candidate in
        // this basis dir", not just ENOENT, exactly as the regular-file lookup
        // above already treats it. An alt-dest arg that is not a directory yields
        // ENOTDIR here, and aborting on it would fail a run upstream completes.
        let Ok(candidate_metadata) = basis_stat(&candidate) else {
            continue;
        };
        if !candidate_metadata.file_type().is_symlink() {
            continue;
        }
        // upstream: generator.c:642-646 quick_check_ok() FT_SYMLINK - a readlink
        // that fails (`len <= 0`) returns 0, which try_dests_non() turns into a
        // `continue`. The unreadable basis link is skipped, never fatal.
        match fs::read_link(&candidate) {
            Ok(basis_target) if basis_target == target => {
                return Ok(Some(candidate_metadata));
            }
            Ok(_) | Err(_) => continue,
        }
    }
    Ok(None)
}

/// Locates an alternate-basis directory at `relative` (`--copy-dest`,
/// `--link-dest`, or `--compare-dest`).
///
/// Returns the basis metadata when any reference contains a directory at
/// `relative`. A directory matched against any basis itemizes as a local change
/// (`cd`) compared to the basis rather than as a new entry (`cd+++++++++`);
/// directories are never hard-linked, so all three kinds behave identically
/// here.
///
/// upstream: generator.c:1117-1148 try_dests_non() - a match itemizes with
/// ITEM_LOCAL_CHANGE and never sets ITEM_IS_NEW (the LINK_DEST hard-link branch
/// is skipped for directories at line 1126, and COMPARE_DEST forces
/// ITEM_LOCAL_CHANGE for directories at line 1140).
pub(crate) fn find_copy_dest_basis(
    context: &CopyContext<'_>,
    destination: &Path,
    relative: &Path,
) -> Result<Option<fs::Metadata>, LocalCopyError> {
    // An empty `relative` is the transfer root: the basis is the reference
    // directory itself, resolved from the destination root. Unlike the file and
    // symlink lookups, the directory lookup must handle this case so the `./`
    // row itemizes against the basis root.
    for reference in context.reference_directories() {
        let candidate = resolve_reference_candidate(reference.path(), relative, destination);
        // upstream: generator.c:1227 - `if (basis_link_stat(cmpbuf, &sxp->st) < 0
        // || ...) continue;`. ANY stat failure means "no candidate in this basis
        // dir", not just ENOENT. This is the transfer-root (`./`) lookup, so an
        // alt-dest arg naming a plain file yields ENOTDIR here on the very first
        // entry; aborting would fail a run upstream completes normally, having
        // merely warned once from check_alt_basis_dirs().
        let Ok(candidate_metadata) = basis_stat(&candidate) else {
            continue;
        };
        if candidate_metadata.file_type().is_dir() {
            return Ok(Some(candidate_metadata));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_absolute_base_ignores_destination() {
        let base = Path::new("/absolute/ref");
        let relative = Path::new("file.txt");
        let destination = Path::new("/some/other/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/absolute/ref/file.txt"));
    }

    #[test]
    fn resolve_absolute_base_with_nested_relative() {
        let base = Path::new("/ref");
        let relative = Path::new("dir/subdir/file.txt");
        let destination = Path::new("/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/ref/dir/subdir/file.txt"));
    }

    #[test]
    fn resolve_relative_base_computes_from_destination() {
        let base = Path::new("../backup");
        let relative = Path::new("file.txt");
        let destination = Path::new("/home/user/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        // destination "/home/user/dest" -> pop 1 level (for relative depth 1) -> "/home/user"
        // then join "../backup" -> "/home/user/../backup" -> normalized "/home/backup"
        // then join "file.txt" -> "/home/backup/file.txt"
        assert_eq!(result, PathBuf::from("/home/backup/file.txt"));
    }

    #[test]
    fn resolve_relative_base_with_deeper_relative_path() {
        let base = Path::new("ref");
        let relative = Path::new("a/b/c/file.txt");
        let destination = Path::new("/x/y/z/dest");
        // depth of relative is 4, so pop 4 levels from destination
        // "/x/y/z/dest" -> "/x/y/z" -> "/x/y" -> "/x" -> "/"
        // then join "ref" -> "/ref"
        // then join "a/b/c/file.txt" -> "/ref/a/b/c/file.txt"
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/ref/a/b/c/file.txt"));
    }

    #[test]
    fn resolve_relative_base_single_component() {
        let base = Path::new("backup");
        let relative = Path::new("file.txt");
        let destination = Path::new("/dest/path");
        // depth 1, pop 1 from "/dest/path" -> "/dest"
        // join "backup" -> "/dest/backup"
        // join "file.txt" -> "/dest/backup/file.txt"
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/dest/backup/file.txt"));
    }

    #[test]
    fn resolve_empty_relative_path() {
        let base = Path::new("/ref");
        let relative = Path::new("");
        let destination = Path::new("/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/ref"));
    }

    #[test]
    fn resolve_relative_base_with_empty_relative() {
        let base = Path::new("backup");
        let relative = Path::new("");
        let destination = Path::new("/dest");
        // empty relative has 0 components, pop 0 times
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/dest/backup"));
    }

    // POSIX-absolute path: `/ref/../other` is only absolute on Unix. On Windows
    // it lacks a drive letter, so `is_absolute()` is false and the relative
    // branch lexically normalizes away the `..`. Gate to Unix where the absolute
    // branch is exercised; real Windows paths (`C:\...`) hit the same branch.
    #[cfg(unix)]
    #[test]
    fn resolve_dotdot_in_base() {
        let base = Path::new("/ref/../other");
        let relative = Path::new("file.txt");
        let destination = Path::new("/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        // base is absolute, so just join
        assert_eq!(result, PathBuf::from("/ref/../other/file.txt"));
    }

    #[test]
    fn reference_decision_skip_variant() {
        let path = PathBuf::from("/compare/basis");
        let decision = ReferenceDecision::Skip(path.clone());
        match decision {
            ReferenceDecision::Skip(p) => assert_eq!(p, path),
            _ => panic!("Expected Skip variant"),
        }
    }

    #[test]
    fn reference_decision_copy_variant() {
        let path = PathBuf::from("/some/path");
        let decision = ReferenceDecision::Copy(path.clone());
        match decision {
            ReferenceDecision::Copy(p) => assert_eq!(p, path),
            _ => panic!("Expected Copy variant"),
        }
    }

    #[test]
    fn reference_decision_link_variant() {
        let path = PathBuf::from("/link/target");
        let decision = ReferenceDecision::Link(path.clone());
        match decision {
            ReferenceDecision::Link(p) => assert_eq!(p, path),
            _ => panic!("Expected Link variant"),
        }
    }

    #[test]
    fn reference_query_fields_accessible() {
        let dest = PathBuf::from("/dest");
        let rel = PathBuf::from("relative");
        let src = PathBuf::from("/src");
        let meta = fs::metadata(".").unwrap_or_else(|_| fs::metadata("/").unwrap());
        let metadata_options = MetadataOptions::default();

        let query = ReferenceQuery {
            destination: &dest,
            relative: &rel,
            source: &src,
            metadata: &meta,
            size_only: true,
            ignore_times: false,
            checksum: true,
            metadata_options: &metadata_options,
            preserve_xattrs: false,
        };

        assert_eq!(query.destination, Path::new("/dest"));
        assert_eq!(query.relative, Path::new("relative"));
        assert_eq!(query.source, Path::new("/src"));
        assert!(query.size_only);
        assert!(!query.ignore_times);
        assert!(query.checksum);
        assert!(!query.preserve_xattrs);
    }
}
