//! Handling of reference directories and link-dest decisions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ::metadata::MetadataOptions;

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

/// Reports whether a reference basis already carries the source's preserved
/// attributes, mirroring upstream `generator.c:468 unchanged_attrs()`.
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
///
// upstream: generator.c:468-502 unchanged_attrs - perms/ownership/time/xattr.
pub(crate) fn reference_attrs_unchanged(
    basis: &Path,
    source: &Path,
    source_meta: &fs::Metadata,
    options: &MetadataOptions,
    preserve_xattrs: bool,
) -> bool {
    let Ok(basis_meta) = fs::symlink_metadata(basis) else {
        return false;
    };

    // A --chmod tweak changes the intended mode away from the source's, so
    // the basis (which carries the untweaked mode) can never be a
    // match_level-3 attrs match; force a reapply.
    if options.chmod().is_some() {
        return false;
    }

    // upstream: generator.c:485 any_time_differs - the mtime must match for a
    // level-3 basis. Compared through `SystemTime` (like the quick-check
    // `unchanged_file` path) so the check holds on every platform, not just Unix
    // where `st_mtime` is available.
    if options.times() {
        match (source_meta.modified(), basis_meta.modified()) {
            (Ok(source_time), Ok(basis_time)) if source_time == basis_time => {}
            _ => return false,
        }
    }

    // upstream: generator.c:487 perms_differ - Unix compares the full permission
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
/// (generator.c:995-1054):
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
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference file",
                    candidate,
                    error,
                ));
            }
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
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference symlink",
                    candidate,
                    error,
                ));
            }
        };
        if !candidate_metadata.file_type().is_symlink() {
            continue;
        }
        match fs::read_link(&candidate) {
            Ok(basis_target) if basis_target == target => {
                return Ok(Some(candidate_metadata));
            }
            Ok(_) => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "read reference symlink",
                    candidate,
                    error,
                ));
            }
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
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference directory",
                    candidate,
                    error,
                ));
            }
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
