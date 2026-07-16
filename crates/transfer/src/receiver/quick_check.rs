//! Quick-check logic for determining whether files need transfer.
//!
//! Implements upstream rsync's `quick_check_ok()` algorithm comparing
//! destination stat against source file list entries, plus reference
//! directory handling and file checksum comparison.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use logging::info_log;
use protocol::flist::FileEntry;

use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};
use crate::delta_apply::ChecksumVerifier;

use metadata::{
    AclIdMapper, MetadataOptions, ModifyWindow, apply_metadata_with_cached_stat, metadata_unchanged,
};
use protocol::acl::AclCache;

use super::apply_acls_from_receiver_cache;

/// Returns true if this file entry is a hardlink follower that should be
/// created as a hard link rather than transferred via delta.
///
/// A follower has XMIT_HLINKED set but NOT XMIT_HLINK_FIRST. Leaders have
/// both flags set. Entries without hardlink flags return false.
///
/// # Upstream Reference
///
/// - `generator.c:1539` - `F_HLINK_NOT_FIRST(file)` check
/// - `hlink.c:284` - `hard_link_check()` called for non-first entries
pub(super) fn is_hardlink_follower(entry: &FileEntry) -> bool {
    entry.hlinked() && !entry.hlink_first()
}

/// Pure-function quick-check: compares destination stat against source entry.
///
/// Returns `true` when the destination file matches the source entry (skip transfer).
///
/// Follows upstream `generator.c:617 quick_check_ok()` evaluation order:
/// 1. Size mismatch - always needs transfer
/// 2. `always_checksum` - compute file checksum and compare (ignores mtime)
/// 3. `size_only` - size matched, skip transfer
/// 4. `!preserve_times` (implies `ignore_times`) - force transfer
/// 5. mtime comparison, tolerating `--modify-window` seconds of drift
pub(super) fn quick_check_matches(
    entry: &FileEntry,
    dest_path: &Path,
    dest_meta: &fs::Metadata,
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
    modify_window: ModifyWindow,
) -> bool {
    // upstream: generator.c:621 - size check first
    if dest_meta.len() != entry.size() {
        return false;
    }
    // upstream: generator.c:626 - always_checksum compares file checksums
    // instead of relying on mtime. Takes priority over size_only and ignore_times.
    if let Some(algorithm) = always_checksum {
        return match entry.checksum() {
            Some(expected) => {
                file_checksum_matches(dest_path, dest_meta.len(), algorithm, expected)
            }
            None => false,
        };
    }
    // upstream: generator.c:632 - `if (size_only) return 1;` after size match
    if size_only {
        return true;
    }
    // upstream: generator.c:635 - ignore_times forces transfer
    if !preserve_times {
        return false;
    }
    // upstream: generator.c:645 - `mtime_differs()` -> `same_time()` applies the
    // `--modify-window` tolerance. A negative window compares nanoseconds too
    // (util1.c:1482), so pass the sub-second component from both sides.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        modify_window.same_time(
            dest_meta.mtime(),
            dest_meta.mtime_nsec() as u32,
            entry.mtime(),
            entry.mtime_nsec(),
        )
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| {
                modify_window.same_time(
                    d.as_secs() as i64,
                    d.subsec_nanos(),
                    entry.mtime(),
                    entry.mtime_nsec(),
                )
            })
    }
}

/// Returns `true` when the destination file's mtime is strictly newer than the source.
///
/// Used by `--update` (`-u`) to skip files where the destination is already newer.
///
/// upstream: generator.c:1709 - `file->modtime - sx.st.st_mtime < modify_window`
/// with modify_window=0, this simplifies to `dest_mtime > source_mtime`.
pub(super) fn dest_mtime_newer(dest_meta: &fs::Metadata, source_entry: &FileEntry) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() > source_entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| (d.as_secs() as i64) > source_entry.mtime())
    }
}

/// Collapses a file type into upstream rsync's `get_file_type()` category.
///
/// Upstream classifies modes into `FT_REG`, `FT_DIR`, `FT_SYMLINK`,
/// `FT_SPECIAL` (fifo/socket) and `FT_DEVICE` (block/char). The `--update`
/// same-type guard compares these collapsed categories, so block vs char
/// devices both count as `FT_DEVICE` and fifo vs socket both count as
/// `FT_SPECIAL`.
///
/// upstream: generator.c:608 - `get_file_type()`.
fn file_type_category(mode: u32) -> Option<u8> {
    use protocol::flist::FileType;
    FileType::from_mode(mode).map(|ft| match ft {
        FileType::Regular => 1,                            // FT_REG
        FileType::Directory => 2,                          // FT_DIR
        FileType::Symlink => 3,                            // FT_SYMLINK
        FileType::Fifo | FileType::Socket => 4,            // FT_SPECIAL
        FileType::BlockDevice | FileType::CharDevice => 5, // FT_DEVICE
    })
}

/// Returns `true` when the destination's actual file type matches the source
/// entry's type, per upstream's collapsed `get_file_type()` categories.
///
/// The destination is inspected with `symlink_metadata` (lstat), mirroring
/// upstream's `link_stat`, so a destination symlink is classified as a symlink
/// rather than as its target. Used by the `--update` skip so a newer
/// destination only suppresses the transfer when it is the same type as the
/// source; a type mismatch (e.g. dest symlink vs source regular file) always
/// transfers.
///
/// upstream: generator.c:1721 - `update_only > 0 && statret == 0
/// && stype == ftype && file->modtime - sx.st.st_mtime < modify_window`.
pub(super) fn dest_type_matches_source(dest_path: &Path, source_entry: &FileEntry) -> bool {
    let Some(source_category) = file_type_category(source_entry.mode()) else {
        return false;
    };
    match fs::symlink_metadata(dest_path) {
        Ok(dest_meta) => {
            let dest_mode = mode_from_metadata(&dest_meta);
            file_type_category(dest_mode) == Some(source_category)
        }
        // No lstat result means the dest vanished; upstream treats a failed
        // stat as statret != 0, which never enters the same-type skip.
        Err(_) => false,
    }
}

/// Extracts the raw `st_mode` bits from filesystem metadata.
///
/// On unix the mode is read directly. On other platforms only the coarse file
/// type (regular / dir / symlink) is available, so it is reconstructed into the
/// matching `S_IFMT` bits.
fn mode_from_metadata(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.mode()
    }
    #[cfg(not(unix))]
    {
        let ft = meta.file_type();
        if ft.is_symlink() {
            0o120000 // S_IFLNK
        } else if ft.is_dir() {
            0o040000 // S_IFDIR
        } else {
            0o100000 // S_IFREG
        }
    }
}

/// The strength of an alternate-basis match, mirroring upstream's `match_level`.
///
/// Upstream `try_dests_reg` (generator.c:960) tracks a `match_level` while
/// scanning `basis_dir[]`. Levels 0 (nothing found) and 1 (a regular file that
/// fails `quick_check_ok`, usable only as a delta basis) are not handled
/// locally by oc, so this enum models only the two levels that produce a
/// finished destination file:
///
/// - [`MatchLevel::Content`] = upstream match_level 2: the basis passes
///   `quick_check_ok` (size + mtime, or checksum) but at least one preserved
///   attribute differs.
/// - [`MatchLevel::Exact`] = upstream match_level 3: the basis passes
///   `quick_check_ok` *and* `unchanged_attrs()` - every preserved attribute
///   matches.
///
/// `Exact > Content` so the scan can keep the strongest match.
///
/// # Upstream Reference
///
/// - `generator.c:960-983` - the `match_level` state machine and `best_match`
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchLevel {
    /// upstream match_level 2: quick-check passes, preserved attrs differ.
    Content,
    /// upstream match_level 3: quick-check passes and `unchanged_attrs()`.
    Exact,
}

/// The best alternate-basis match found across all `basis_dir[]` entries.
struct ReferenceMatch<'a> {
    /// The reference directory whose basis file was selected (`best_match`).
    ref_dir: &'a ReferenceDirectory,
    /// Resolved path of the basis file inside [`ReferenceMatch::ref_dir`].
    ref_path: PathBuf,
    /// The match strength, driving the local action (skip / hard-link / copy).
    level: MatchLevel,
}

/// Scans every reference directory and returns the strongest basis match,
/// mirroring upstream's `best_match` selection in `try_dests_reg`.
///
/// Upstream keeps the FIRST directory that reaches the highest `match_level`: a
/// later directory only wins by strictly exceeding the current level, and a
/// level-3 (attrs-exact) match ends the scan immediately (generator.c:967-983).
/// Directories whose basis file is missing, non-regular, or fails
/// `quick_check_ok` contribute at most match_level 1 and are ignored here.
///
/// The basis-dir entry is stat'd with `lstat` by default to mirror upstream
/// `link_stat(cmpbuf, &sxp->st, 0)`. When `copy_links` is set (`-L`), it is
/// stat'd with `stat` so a basis-dir symlink to a regular file is accepted,
/// matching `link_stat()`'s dispatch to `x_stat` (flist.c:234).
///
/// # Upstream Reference
///
/// - `generator.c:963-983` - the `do { ... } while (basis_dir[++j])` scan
/// - `generator.c:965` - `link_stat` + `!S_ISREG` filter
/// - `generator.c:971` - `quick_check_ok` gate (below it stays match_level 1)
/// - `generator.c:977` - `unchanged_attrs()` promotes to match_level 3
#[allow(clippy::too_many_arguments)]
fn best_reference_match<'a>(
    entry: &FileEntry,
    relative_path: &Path,
    reference_directories: &'a [ReferenceDirectory],
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
    modify_window: ModifyWindow,
    copy_links: bool,
    metadata_opts: &MetadataOptions,
) -> Option<ReferenceMatch<'a>> {
    let mut best: Option<ReferenceMatch<'a>> = None;
    for ref_dir in reference_directories {
        let ref_path = ref_dir.path.join(relative_path);
        let stat_result = if copy_links {
            fs::metadata(&ref_path)
        } else {
            fs::symlink_metadata(&ref_path)
        };
        let ref_meta = match stat_result {
            Ok(m) if m.file_type().is_file() => m,
            _ => continue,
        };

        // upstream: generator.c:971 - a basis that fails quick_check_ok is at
        // most match_level 1 (a delta basis), which oc does not consume locally.
        if !quick_check_matches(
            entry,
            &ref_path,
            &ref_meta,
            preserve_times,
            size_only,
            always_checksum,
            modify_window,
        ) {
            continue;
        }

        // upstream: generator.c:977 - unchanged_attrs() distinguishes level 3
        // (every preserved attr equal) from level 2 (content matches, attrs
        // differ). `metadata_unchanged` is oc's `unchanged_attrs` equivalent.
        let level = if metadata_unchanged(entry, metadata_opts, &ref_meta) {
            MatchLevel::Exact
        } else {
            MatchLevel::Content
        };

        // upstream: generator.c:967-981 - best_match keeps the first dir at the
        // highest level; only a strictly higher level replaces it.
        if best.as_ref().is_none_or(|b| level > b.level) {
            let exact = level == MatchLevel::Exact;
            best = Some(ReferenceMatch {
                ref_dir,
                ref_path,
                level,
            });
            // upstream: generator.c:980 - break as soon as a level-3 match is found.
            if exact {
                break;
            }
        }
    }
    best
}

/// Checks reference directories for a file that matches the source entry.
///
/// When the destination file does not exist, this function scans all configured
/// reference directories (`--compare-dest`, `--copy-dest`, `--link-dest`),
/// keeps the strongest [`MatchLevel`] found (upstream's `best_match`), and then
/// performs the action dictated by upstream's `try_dests_reg` matrix:
///
/// - `--link-dest`, [`MatchLevel::Exact`]: hard-link the shared inode; on a
///   hard-link failure, fall back to copying (`goto try_a_copy`).
/// - `--link-dest`, [`MatchLevel::Content`]: attrs differ, so COPY the basis
///   file rather than hard-linking. Hard-linking here would apply the source's
///   attributes onto the shared read-only reference inode, corrupting it.
/// - `--copy-dest`, either level: copy the basis file into the destination.
/// - `--compare-dest`, [`MatchLevel::Exact`]: the file is already correct in
///   the reference tree, so nothing is written to the destination (skip).
/// - `--compare-dest`, [`MatchLevel::Content`]: copy the basis file in so the
///   destination reflects the source's attributes.
///
/// Returns `true` if the entry was handled and should not be transferred.
///
/// # Upstream Reference
///
/// - `generator.c:954` - `try_dests_reg()` iterates `basis_dir[]`
/// - `generator.c:995` - match_level 3 (non-`COPY_DEST`): link-dest hard-links,
///   compare-dest skips; `COPY_DEST` falls through to the copy path
/// - `generator.c:1004-1005` - hard-link failure does `goto try_a_copy`
/// - `generator.c:1029-1051` - match_level >= 2 copies via `copy_altdest_file()`
#[allow(clippy::too_many_arguments)]
pub(super) fn try_reference_dest(
    entry: &FileEntry,
    dest_dir: &Path,
    reference_directories: &[ReferenceDirectory],
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
    modify_window: ModifyWindow,
    copy_links: bool,
    metadata_opts: &MetadataOptions,
    metadata_errors: &mut Vec<(PathBuf, String)>,
    acl_cache: Option<&AclCache>,
    acl_id_map: Option<&AclIdMapper>,
) -> bool {
    if reference_directories.is_empty() {
        return false;
    }

    let relative_path = entry.path();
    let Some(best) = best_reference_match(
        entry,
        relative_path,
        reference_directories,
        preserve_times,
        size_only,
        always_checksum,
        modify_window,
        copy_links,
        metadata_opts,
    ) else {
        return false;
    };

    let dest_path = dest_dir.join(relative_path);
    match best.ref_dir.kind {
        ReferenceDirectoryKind::Compare => {
            // upstream: generator.c:995-1023 - a COMPARE_DEST match at level 3 is
            // already correct, so the file finishes with no local copy and the
            // destination stays absent. At level 2 (attrs differ) it falls
            // through to copy_altdest_file so the destination reflects the
            // source's attributes.
            if best.level == MatchLevel::Exact {
                true
            } else {
                copy_reference_file(
                    entry,
                    &best.ref_path,
                    &dest_path,
                    metadata_opts,
                    metadata_errors,
                    acl_cache,
                    acl_id_map,
                )
            }
        }
        ReferenceDirectoryKind::Link => {
            // upstream: generator.c:995-1014 - LINK_DEST hard-links only at
            // level 3 (unchanged_attrs). A level-2 match, or a hard-link that
            // fails, copies instead (generator.c:1004-1005 `goto try_a_copy`),
            // leaving the read-only reference inode untouched.
            if best.level == MatchLevel::Exact
                && link_reference_file(
                    entry,
                    &best.ref_path,
                    &dest_path,
                    metadata_opts,
                    metadata_errors,
                    acl_cache,
                    acl_id_map,
                )
            {
                true
            } else {
                copy_reference_file(
                    entry,
                    &best.ref_path,
                    &dest_path,
                    metadata_opts,
                    metadata_errors,
                    acl_cache,
                    acl_id_map,
                )
            }
        }
        ReferenceDirectoryKind::Copy => {
            // upstream: generator.c:995 (COPY_DEST bypasses the level-3 skip)
            // + 1029 - COPY_DEST copies the basis file at both level 2 and 3.
            copy_reference_file(
                entry,
                &best.ref_path,
                &dest_path,
                metadata_opts,
                metadata_errors,
                acl_cache,
                acl_id_map,
            )
        }
    }
}

/// Applies the source entry's metadata and ACLs onto a freshly materialised
/// destination (hard link or copy), recording any failures in `metadata_errors`.
///
/// The cached stat is deliberately `None`: the destination was just created, so
/// its on-disk metadata does not yet match the entry and an extra stat would be
/// wasted.
fn apply_altdest_metadata(
    entry: &FileEntry,
    dest_path: &Path,
    metadata_opts: &MetadataOptions,
    metadata_errors: &mut Vec<(PathBuf, String)>,
    acl_cache: Option<&AclCache>,
    acl_id_map: Option<&AclIdMapper>,
) {
    if let Err(e) = apply_metadata_with_cached_stat(dest_path, entry, metadata_opts, None) {
        metadata_errors.push((dest_path.to_path_buf(), e.to_string()));
    }
    if let Err(e) =
        apply_acls_from_receiver_cache(dest_path, entry, acl_cache, acl_id_map, !entry.is_symlink())
    {
        metadata_errors.push((dest_path.to_path_buf(), e.to_string()));
    }
}

/// Hard-links `ref_path` (a `--link-dest` basis) into `dest_path`, then applies
/// the source entry's metadata. Returns `true` on a successful link.
///
/// Only called at [`MatchLevel::Exact`], where every preserved attribute
/// already matches, so applying metadata onto the shared inode is a no-op that
/// cannot corrupt the reference tree. A failed link returns `false` so the
/// caller can fall back to copying (upstream `goto try_a_copy`).
///
/// upstream: generator.c:1003-1014 - `hard_link_one()` then `set_file_attrs()`
fn link_reference_file(
    entry: &FileEntry,
    ref_path: &Path,
    dest_path: &Path,
    metadata_opts: &MetadataOptions,
    metadata_errors: &mut Vec<(PathBuf, String)>,
    acl_cache: Option<&AclCache>,
    acl_id_map: Option<&AclIdMapper>,
) -> bool {
    if let Some(parent) = dest_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Try io_uring LINKAT on Linux 5.15+, fall back to std::fs::hard_link.
    if fast_io::hard_link(ref_path, dest_path).is_err() {
        return false;
    }
    apply_altdest_metadata(
        entry,
        dest_path,
        metadata_opts,
        metadata_errors,
        acl_cache,
        acl_id_map,
    );
    true
}

/// Copies `ref_path` (an alternate-basis file) into `dest_path` and applies the
/// source entry's metadata. Returns `true` when the copy succeeds.
///
/// On failure it emits the upstream `INFO_GTE(COPY, 1)` notice and returns
/// `false` so the caller requests a normal transfer.
///
/// upstream: generator.c:912-946 - `copy_altdest_file()`
fn copy_reference_file(
    entry: &FileEntry,
    ref_path: &Path,
    dest_path: &Path,
    metadata_opts: &MetadataOptions,
    metadata_errors: &mut Vec<(PathBuf, String)>,
    acl_cache: Option<&AclCache>,
    acl_id_map: Option<&AclIdMapper>,
) -> bool {
    if let Some(parent) = dest_path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match fs::copy(ref_path, dest_path) {
        Ok(_) => {
            apply_altdest_metadata(
                entry,
                dest_path,
                metadata_opts,
                metadata_errors,
                acl_cache,
                acl_id_map,
            );
            true
        }
        Err(error) => {
            // upstream: generator.c:919 - rsyserr(FINFO, errno,
            // "copy_file %s => %s", full_fname(src), copy_to) under
            // INFO_GTE(COPY, 1). The flag is part of info_verbosity[1]
            // (options.c:241) so this fires at `-v` or `--info=COPY`. Wording
            // mirrors upstream's rsyserr format: `copy_file SRC => DST: ERRSTR (ERRNO)`.
            let errno = error.raw_os_error().unwrap_or(0);
            info_log!(
                Copy,
                1,
                "copy_file {} => {}: {} ({})",
                ref_path.display(),
                dest_path.display(),
                io_error_message(&error),
                errno
            );
            false
        }
    }
}

/// Strips the trailing `" (os error N)"` suffix that `std::io::Error::Display`
/// appends to OS errors so the rendered message matches upstream `rsyserr`'s
/// `strerror(errno)` form. Non-OS errors are returned as-is.
fn io_error_message(error: &std::io::Error) -> String {
    let display = error.to_string();
    if let Some(errno) = error.raw_os_error() {
        let suffix = format!(" (os error {errno})");
        if let Some(trimmed) = display.strip_suffix(&suffix) {
            return trimmed.to_string();
        }
    }
    display
}

/// Computes a file's checksum and compares it against an expected value.
///
/// Used by `--checksum` (`-c`) mode to compare file contents instead of
/// mtime+size quick-check. Returns `true` when checksums match (skip transfer).
///
/// upstream: checksum.c:402 `file_checksum()` - plain hash, no seed
fn file_checksum_matches(
    path: &Path,
    file_size: u64,
    algorithm: protocol::ChecksumAlgorithm,
    expected: &[u8],
) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut hasher = ChecksumVerifier::for_algorithm(algorithm);
    // upstream: rsync.h:159 MAX_MAP_SIZE = 256*1024
    let mut buf = vec![0u8; 256 * 1024];
    let mut remaining = file_size;
    while remaining > 0 {
        let to_read = buf.len().min(remaining as usize);
        if file.read_exact(&mut buf[..to_read]).is_err() {
            return false;
        }
        hasher.update(&buf[..to_read]);
        remaining -= to_read as u64;
    }
    let mut digest = [0u8; ChecksumVerifier::MAX_DIGEST_LEN];
    let len = hasher.finalize_into(&mut digest);
    // upstream: flist_csum_len determines comparison length
    let cmp_len = expected.len().min(len);
    digest[..cmp_len] == expected[..cmp_len]
}

/// Returns `true` if any component of the path is `..`.
///
/// This mirrors upstream rsync's `clean_fname(thisname, CFN_REFUSE_DOT_DOT_DIRS)`
/// check that rejects paths containing parent-directory references, preventing
/// directory traversal attacks from a malicious sender.
///
/// # Upstream Reference
///
/// - `util1.c`: `clean_fname()` with `CFN_REFUSE_DOT_DOT_DIRS`
pub(super) fn path_contains_dot_dot(path: &Path) -> bool {
    path.components().any(|c| matches!(c, Component::ParentDir))
}

#[cfg(test)]
mod io_uring_linkat_tests {
    use std::fs;

    /// Verifies `fast_io::hard_link` creates a valid hard link regardless of
    /// whether io_uring handles it or `std::fs::hard_link` does.
    #[test]
    fn hard_link_via_io_uring_or_fallback_in_quick_check() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("linkdest_src.txt");
        let dst = dir.path().join("linkdest_dst.txt");

        fs::write(&src, b"link-dest payload").unwrap();

        fast_io::hard_link(&src, &dst).unwrap();

        assert!(src.exists());
        assert!(dst.exists());
        assert_eq!(fs::read(&dst).unwrap(), b"link-dest payload");
    }

    /// Verifies the fallback correctly fails when the source does not exist.
    #[test]
    fn hard_link_fails_for_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("missing.txt");
        let dst = dir.path().join("dst.txt");

        let result = fast_io::hard_link(&src, &dst);
        assert!(result.is_err());
    }
}

/// Pinning tests for `--info=COPY` emissions on `--copy-dest` failures.
///
/// upstream: generator.c:919 - `INFO_GTE(COPY, 1)` gates an `rsyserr` call in
/// `copy_altdest_file()` whose wording reads:
///   `copy_file SRC => DST: STRERROR (ERRNO)`
/// COPY sits in `info_verbosity[1]` (options.c:241) so the flag is enabled at
/// `-v` and any explicit `--info=COPY`.
#[cfg(unix)]
#[cfg(test)]
mod info_copy_emission_tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use logging::{DiagnosticEvent, InfoFlag, VerbosityConfig, drain_events, init};
    use metadata::MetadataOptions;
    use protocol::flist::FileEntry;

    use super::{ModifyWindow, try_reference_dest};
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    fn copy_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Info {
                    flag: InfoFlag::Copy,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn init_copy_level(level: u8) {
        let mut cfg = VerbosityConfig::from_verbose_level(0);
        cfg.info.copy = level;
        init(cfg);
        let _ = drain_events();
    }

    /// Verifies that when `--copy-dest` cannot write the destination,
    /// `try_reference_dest` emits the upstream-format COPY notice via the
    /// diagnostic queue.
    #[test]
    fn copy_dest_failure_emits_info_copy_notice() {
        init_copy_level(1);

        let temp = tempfile::tempdir().expect("tempdir");
        let ref_dir = temp.path().join("ref");
        fs::create_dir_all(&ref_dir).expect("create ref dir");

        // Populate the alternate-base file the receiver wants to copy.
        let relative = PathBuf::from("payload.bin");
        let payload = b"alt-base contents";
        fs::write(ref_dir.join(&relative), payload).expect("write ref file");

        // Make the destination directory read-only so `fs::copy` fails.
        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");
        let mut perms = fs::metadata(&dest_dir).expect("dest meta").permissions();
        perms.set_mode(0o500);
        fs::set_permissions(&dest_dir, perms).expect("chmod dest");

        let entry = FileEntry::new_file(relative.clone(), payload.len() as u64, 0o644);
        let reference = ReferenceDirectory {
            kind: ReferenceDirectoryKind::Copy,
            path: ref_dir.clone(),
        };
        let metadata_opts = MetadataOptions::default();
        let mut metadata_errors = Vec::new();

        let handled = try_reference_dest(
            &entry,
            &dest_dir,
            std::slice::from_ref(&reference),
            false,
            true,
            None,
            ModifyWindow::from_secs(0),
            false,
            &metadata_opts,
            &mut metadata_errors,
            None,
            None,
        );

        // Restore writable permissions so tempdir cleanup succeeds.
        let mut restore = fs::metadata(&dest_dir).expect("dest meta").permissions();
        restore.set_mode(0o700);
        let _ = fs::set_permissions(&dest_dir, restore);

        assert!(
            !handled,
            "expected failed alt-base copy to bubble up as unhandled"
        );

        let messages = copy_messages();
        assert!(
            messages.iter().any(|m| m.starts_with("copy_file ")
                && m.contains(" => ")
                && m.contains("payload.bin")),
            "expected upstream-format COPY,1 notice; got {messages:?}"
        );
    }

    /// Verifies that `--info=nocopy` (level 0) suppresses the emission,
    /// mirroring upstream's `INFO_GTE(COPY, 1)` gate.
    #[test]
    fn nocopy_suppresses_info_copy_notice() {
        init_copy_level(0);

        let temp = tempfile::tempdir().expect("tempdir");
        let ref_dir = temp.path().join("ref");
        fs::create_dir_all(&ref_dir).expect("create ref dir");
        let relative = PathBuf::from("muted.bin");
        let payload = b"silent payload";
        fs::write(ref_dir.join(&relative), payload).expect("write ref file");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");
        let mut perms = fs::metadata(&dest_dir).expect("dest meta").permissions();
        perms.set_mode(0o500);
        fs::set_permissions(&dest_dir, perms).expect("chmod dest");

        let entry = FileEntry::new_file(relative.clone(), payload.len() as u64, 0o644);
        let reference = ReferenceDirectory {
            kind: ReferenceDirectoryKind::Copy,
            path: ref_dir,
        };
        let metadata_opts = MetadataOptions::default();
        let mut metadata_errors = Vec::new();

        let _ = try_reference_dest(
            &entry,
            &dest_dir,
            std::slice::from_ref(&reference),
            false,
            true,
            None,
            ModifyWindow::from_secs(0),
            false,
            &metadata_opts,
            &mut metadata_errors,
            None,
            None,
        );

        let mut restore = fs::metadata(&dest_dir).expect("dest meta").permissions();
        restore.set_mode(0o700);
        let _ = fs::set_permissions(&dest_dir, restore);

        // Restore the default so later tests in this thread see the upstream baseline.
        init(VerbosityConfig::from_verbose_level(0));
        let _ = drain_events();

        assert!(
            copy_messages().is_empty(),
            "COPY notice must be gated at level 1"
        );
    }
}

/// Regression tests for symlink handling in basis-dir lookups under
/// `--copy-dest` / `--link-dest` / `--compare-dest`.
///
/// upstream: `generator.c:953` — `link_stat(cmpbuf, &sxp->st, 0)` followed by
/// `!S_ISREG(sxp->st.st_mode)` filters out a basis-dir entry that is itself a
/// symlink (unless `copy_links` is set, in which case `link_stat()` falls
/// through to `x_stat()` per `flist.c:234`). Without this gate the receiver
/// would silently copy or hard-link from a symlink target it should not have
/// consumed, diverging from upstream over remote-shell transports where the
/// upstream `alt-dest.test` exercises the same scenario via `lsh.sh`.
#[cfg(unix)]
#[cfg(test)]
mod symlink_basis_tests {
    use std::fs;
    use std::os::unix;
    use std::path::PathBuf;

    use metadata::MetadataOptions;
    use protocol::flist::FileEntry;

    use super::{ModifyWindow, try_reference_dest};
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    fn setup_symlink_basis() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let ref_dir = temp.path().join("basis");
        fs::create_dir_all(&ref_dir).expect("create basis dir");

        // Real regular file outside the basis-dir entry the receiver will probe.
        let target = ref_dir.join("real-target");
        fs::write(&target, b"basis payload").expect("write basis target");

        // Basis-dir entry that matches the source name is itself a symlink to
        // the regular file. Upstream's `link_stat()` lstats this and skips it
        // because S_ISLNK != S_ISREG. oc-rsync must mirror that behaviour.
        let basis_entry = ref_dir.join("payload.bin");
        unix::fs::symlink("real-target", &basis_entry).expect("create basis symlink");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");

        (temp, ref_dir, dest_dir)
    }

    fn run(
        kind: ReferenceDirectoryKind,
        ref_dir: &std::path::Path,
        dest_dir: &std::path::Path,
        copy_links: bool,
    ) -> bool {
        let entry = FileEntry::new_file(
            PathBuf::from("payload.bin"),
            b"basis payload".len() as u64,
            0o644,
        );
        let reference = ReferenceDirectory {
            kind,
            path: ref_dir.to_path_buf(),
        };
        let metadata_opts = MetadataOptions::default();
        let mut metadata_errors = Vec::new();

        try_reference_dest(
            &entry,
            dest_dir,
            std::slice::from_ref(&reference),
            false,
            true,
            None,
            ModifyWindow::from_secs(0),
            copy_links,
            &metadata_opts,
            &mut metadata_errors,
            None,
            None,
        )
    }

    /// Without `--copy-links`, a basis-dir symlink must NOT match. Upstream
    /// returns `-1` from `try_dests_reg` so the receiver requests a transfer
    /// from the sender. oc-rsync must mirror this to keep wire-byte parity
    /// over remote-shell transports (upstream `alt-dest.test`).
    #[test]
    fn copy_dest_skips_symlink_basis_entry_without_copy_links() {
        let (_tmp, ref_dir, dest_dir) = setup_symlink_basis();

        let handled = run(ReferenceDirectoryKind::Copy, &ref_dir, &dest_dir, false);

        assert!(
            !handled,
            "basis-dir symlink must be skipped without --copy-links"
        );
        assert!(
            !dest_dir.join("payload.bin").exists(),
            "destination must not be populated from a symlink basis"
        );
    }

    /// `--link-dest` with a symlink basis must also be skipped: upstream's
    /// `try_dests_reg` filters before reaching `hard_link_one()`.
    #[test]
    fn link_dest_skips_symlink_basis_entry_without_copy_links() {
        let (_tmp, ref_dir, dest_dir) = setup_symlink_basis();

        let handled = run(ReferenceDirectoryKind::Link, &ref_dir, &dest_dir, false);

        assert!(
            !handled,
            "basis-dir symlink must be skipped without --copy-links"
        );
        assert!(
            !dest_dir.join("payload.bin").exists(),
            "destination must not be hard-linked from a symlink basis"
        );
    }

    /// `--compare-dest` mirrors the same filter: a symlink basis cannot mark
    /// the source entry as up-to-date.
    #[test]
    fn compare_dest_skips_symlink_basis_entry_without_copy_links() {
        let (_tmp, ref_dir, dest_dir) = setup_symlink_basis();

        let handled = run(ReferenceDirectoryKind::Compare, &ref_dir, &dest_dir, false);

        assert!(
            !handled,
            "compare-dest must not treat symlink basis as up-to-date"
        );
    }

    /// With `--copy-links` (`-L`), upstream's `link_stat()` dispatches to
    /// `x_stat()` which follows the symlink. A basis-dir symlink pointing at
    /// a matching regular file is accepted as a regular-file basis.
    #[test]
    fn copy_dest_follows_symlink_basis_entry_with_copy_links() {
        let (_tmp, ref_dir, dest_dir) = setup_symlink_basis();

        let handled = run(ReferenceDirectoryKind::Copy, &ref_dir, &dest_dir, true);

        assert!(
            handled,
            "with --copy-links, the symlink basis must resolve to a regular file"
        );
        assert_eq!(
            fs::read(dest_dir.join("payload.bin")).expect("dest file"),
            b"basis payload"
        );
    }

    /// Sanity check: a regular-file basis-dir entry still matches in the
    /// default `copy_links=false` path. This guards against an over-eager fix
    /// that lstats the entry but then fails to recognise a regular file.
    #[test]
    fn copy_dest_matches_regular_basis_entry_without_copy_links() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ref_dir = temp.path().join("basis");
        fs::create_dir_all(&ref_dir).expect("create basis dir");
        fs::write(ref_dir.join("payload.bin"), b"basis payload").expect("write basis");

        let dest_dir = temp.path().join("dest");
        fs::create_dir_all(&dest_dir).expect("create dest dir");

        let handled = run(ReferenceDirectoryKind::Copy, &ref_dir, &dest_dir, false);

        assert!(handled, "regular-file basis must still be accepted");
        assert_eq!(
            fs::read(dest_dir.join("payload.bin")).expect("dest file"),
            b"basis payload"
        );
    }
}

/// Regression tests pinning the `--update` (`-u`) same-type skip guard.
///
/// upstream: generator.c:1721 - the "newer dest -> skip" logic is guarded by
/// `stype == ftype`, so a newer destination only suppresses the transfer when
/// it is the SAME file type as the source. A type mismatch (e.g. a newer dest
/// symlink over a source regular file) always transfers regardless of mtime.
/// These tests encode WHY: without the guard, `-u` silently kept a stale
/// symlink and dropped the source's real content.
#[cfg(unix)]
#[cfg(test)]
mod update_type_guard_tests {
    use std::fs;
    use std::os::unix::fs::symlink;

    use protocol::flist::FileEntry;

    use super::dest_type_matches_source;

    /// A source regular file over a destination symlink is a type MISMATCH, so
    /// the `-u` skip must not fire (upstream `stype != ftype`): transfer wins.
    #[test]
    fn regular_source_over_symlink_dest_is_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("item");
        let target = dir.path().join("target.bin");
        fs::write(&target, b"target payload").expect("write target");
        symlink(&target, &dest).expect("create dest symlink");

        let entry = FileEntry::new_file("item".into(), 42, 0o644);
        assert!(
            !dest_type_matches_source(&dest, &entry),
            "dest symlink vs source regular file must be a type mismatch"
        );
    }

    /// A source regular file over a destination regular file is the SAME type,
    /// so the guard permits the `-u` newer-dest skip (upstream `stype ==
    /// ftype`): mtime alone then decides.
    #[test]
    fn regular_source_over_regular_dest_matches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("item");
        fs::write(&dest, b"dest payload").expect("write dest");

        let entry = FileEntry::new_file("item".into(), 42, 0o644);
        assert!(
            dest_type_matches_source(&dest, &entry),
            "dest regular vs source regular must match so mtime can skip"
        );
    }

    /// The destination is inspected with lstat: a symlink pointing at a regular
    /// file is still classified as a symlink, not its target's type.
    #[test]
    fn symlink_dest_is_not_followed_for_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("item");
        let target = dir.path().join("real.bin");
        fs::write(&target, b"real payload").expect("write target");
        symlink(&target, &dest).expect("create dest symlink");

        // Source is also a symlink -> same type, guard matches.
        let sym_entry = FileEntry::new_symlink("item".into(), target.clone());
        assert!(
            dest_type_matches_source(&dest, &sym_entry),
            "symlink dest vs symlink source must match (lstat, not followed)"
        );
    }

    /// A vanished destination yields no lstat, which upstream treats as
    /// `statret != 0` -> never the same-type skip.
    #[test]
    fn missing_dest_is_not_a_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest = dir.path().join("does-not-exist");

        let entry = FileEntry::new_file("does-not-exist".into(), 42, 0o644);
        assert!(
            !dest_type_matches_source(&dest, &entry),
            "missing dest must not count as a same-type match"
        );
    }
}

/// Regression tests pinning `--modify-window` in the receiver quick-check.
///
/// upstream: generator.c:quick_check_ok() consults `mtime_differs()` ->
/// `util1.c:1478 same_time()` for EVERY transfer, so a remote/daemon pull or
/// push must tolerate `--modify-window` seconds of whole-second mtime drift
/// exactly like the local-copy path. Without threading the window into the
/// receiver, a content-identical file whose mtime differs by <= window is
/// needlessly re-transferred.
#[cfg(unix)]
#[cfg(test)]
mod modify_window_tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    use filetime::{FileTime, set_file_mtime};
    use protocol::flist::FileEntry;

    use super::{ModifyWindow, quick_check_matches};

    /// Builds a dest file at `dest_secs` and a source entry claiming `src_secs`,
    /// both the same size, then runs the quick-check with `preserve_times=true`,
    /// `size_only=false`, no checksum, at the given `window`.
    fn run_window(src_secs: i64, dest_secs: i64, window: ModifyWindow) -> bool {
        run_window_nsec(src_secs, 0, dest_secs, 0, window)
    }

    /// Like [`run_window`] but with explicit nanosecond components, so the
    /// negative-window (nanosecond-exact) path can be exercised.
    fn run_window_nsec(
        src_secs: i64,
        src_nsec: u32,
        dest_secs: i64,
        dest_nsec: u32,
        window: ModifyWindow,
    ) -> bool {
        let dir = tempfile::tempdir().expect("tempdir");
        let dest_path = dir.path().join("payload.bin");
        let payload = b"identical content";
        fs::write(&dest_path, payload).expect("write dest");
        set_file_mtime(&dest_path, FileTime::from_unix_time(dest_secs, dest_nsec))
            .expect("set dest mtime");

        // Read back the on-disk mtime so the assertion reflects what the
        // filesystem actually stored (the quick-check reads dest_meta.mtime()).
        let dest_meta = fs::metadata(&dest_path).expect("dest meta");
        assert_eq!(
            dest_meta.mtime(),
            dest_secs,
            "dest mtime not set as expected"
        );

        let mut entry = FileEntry::new_file("payload.bin".into(), payload.len() as u64, 0o644);
        entry.set_mtime(src_secs, src_nsec);

        quick_check_matches(&entry, &dest_path, &dest_meta, true, false, None, window)
    }

    /// A destination whose mtime is within `--modify-window=2` of the source
    /// (2s apart) is treated as up-to-date and SKIPPED, mirroring same_time()'s
    /// symmetric whole-second tolerance. This is the network-receiver bug fix:
    /// previously the exact `==` compare re-transferred it.
    #[test]
    fn within_window_two_is_skipped() {
        let base = 1_700_000_000;
        // Both directions of drift must skip (same_time is symmetric).
        assert!(
            run_window(base, base + 2, ModifyWindow::from_secs(2)),
            "dest +2s within window must skip"
        );
        assert!(
            run_window(base, base - 2, ModifyWindow::from_secs(2)),
            "dest -2s within window must skip"
        );
        assert!(
            run_window(base, base + 1, ModifyWindow::from_secs(2)),
            "dest +1s within window must skip"
        );
    }

    /// A destination 3s away from the source exceeds `--modify-window=2`, so
    /// same_time() reports the files as different and the receiver transfers.
    #[test]
    fn beyond_window_two_is_transferred() {
        let base = 1_700_000_000;
        assert!(
            !run_window(base, base + 3, ModifyWindow::from_secs(2)),
            "dest +3s beyond window must transfer"
        );
        assert!(
            !run_window(base, base - 3, ModifyWindow::from_secs(2)),
            "dest -3s beyond window must transfer"
        );
    }

    /// With `--modify-window=0` the quick-check requires exact whole-second
    /// equality (same_time() reduces to `f1_sec == f2_sec`): equal mtimes skip,
    /// a one-second delta re-transfers.
    #[test]
    fn zero_window_requires_exact_seconds() {
        let base = 1_700_000_000;
        assert!(
            run_window(base, base, ModifyWindow::from_secs(0)),
            "equal mtimes at window 0 must skip"
        );
        assert!(
            !run_window(base, base + 1, ModifyWindow::from_secs(0)),
            "1s delta at window 0 must transfer"
        );
    }

    /// With `--modify-window=-1` the quick-check requires nanosecond-exact
    /// equality (upstream `modify_window < 0`, util1.c:1482): two files sharing
    /// a whole second but differing in the sub-second component are DIFFERENT
    /// and must be transferred, whereas any non-negative window skips them.
    #[test]
    fn negative_window_requires_nanosecond_exactness() {
        let base = 1_700_000_000;
        let exact = ModifyWindow::from_secs(-1);
        // Same second, differing nanoseconds -> transfer under nsec-exact mode.
        assert!(
            !run_window_nsec(base, 500_000_000, base, 0, exact),
            "sub-second mtime drift must transfer at window -1"
        );
        // Identical seconds and nanoseconds -> still skipped.
        assert!(
            run_window_nsec(base, 0, base, 0, exact),
            "identical mtimes must skip even at window -1"
        );
        // A zero window ignores the same sub-second drift and skips, proving the
        // negative window is what makes the difference observable.
        assert!(
            run_window_nsec(base, 500_000_000, base, 0, ModifyWindow::from_secs(0)),
            "sub-second drift is ignored at window 0"
        );
    }
}

/// Regression tests pinning upstream's `try_dests_reg` match-level matrix for
/// regular files (`generator.c:954-1054`).
///
/// The core data-integrity invariant: `--link-dest` may only hard-link a basis
/// file when EVERY preserved attribute matches (match_level 3). When the content
/// matches but an attribute differs (match_level 2), upstream copies the basis
/// into the destination via `copy_altdest_file` rather than hard-linking, so it
/// never mutates the shared read-only reference inode. `--compare-dest`
/// similarly only treats a basis as up-to-date at level 3; at level 2 it copies
/// the file in so the destination is not left absent. These tests encode WHY:
/// a naive "hard-link on any quick-check match" corrupts the reference tree's
/// attributes through the shared inode, and a naive "compare-dest skips on any
/// match" silently drops attrs-differing files from the destination.
#[cfg(unix)]
#[cfg(test)]
mod alt_dest_match_level_tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Path, PathBuf};

    use filetime::{FileTime, set_file_mtime};
    use metadata::MetadataOptions;
    use protocol::flist::FileEntry;

    use super::{ModifyWindow, try_reference_dest};
    use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};

    /// A fixed mtime shared by basis files and source entries so the quick-check
    /// mtime comparison passes; level differences come from perms, not time,
    /// unless a test opts into `size_only`.
    const MTIME: i64 = 1_700_000_000;

    /// Writes a basis file with explicit content, permissions, and mtime.
    fn write_basis(dir: &Path, name: &str, content: &[u8], mode: u32, mtime: i64) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("write basis file");
        fs::set_permissions(&path, PermissionsExt::from_mode(mode)).expect("chmod basis");
        set_file_mtime(&path, FileTime::from_unix_time(mtime, 0)).expect("set basis mtime");
        path
    }

    /// Builds a source entry with a matching size, perms, and mtime.
    fn source_entry(name: &str, size: u64, mode: u32, mtime: i64) -> FileEntry {
        let mut entry = FileEntry::new_file(PathBuf::from(name), size, mode);
        entry.set_mtime(mtime, 0);
        entry
    }

    #[allow(clippy::too_many_arguments)]
    fn run(
        entry: &FileEntry,
        dest_dir: &Path,
        refs: &[ReferenceDirectory],
        preserve_times: bool,
        size_only: bool,
    ) -> bool {
        let opts = MetadataOptions::default();
        let mut errs = Vec::new();
        try_reference_dest(
            entry,
            dest_dir,
            refs,
            preserve_times,
            size_only,
            None,
            ModifyWindow::from_secs(0),
            false,
            &opts,
            &mut errs,
            None,
            None,
        )
    }

    fn nlink(path: &Path) -> u64 {
        fs::symlink_metadata(path).expect("stat").nlink()
    }

    fn ino(path: &Path) -> u64 {
        fs::symlink_metadata(path).expect("stat").ino()
    }

    /// (a) `--link-dest` at match_level 2 (content matches, mtime differs under
    /// `--size-only`) must COPY the basis into the destination, NOT hard-link it.
    /// Hard-linking would then apply the source's mtime onto the shared reference
    /// inode, corrupting the read-only `--link-dest` tree. The reference file's
    /// link count must stay 1 and the destination must be a distinct inode.
    #[test]
    fn link_dest_level2_copies_and_leaves_reference_inode_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ref_dir = tmp.path().join("link");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir_all(&ref_dir).expect("mk ref");
        fs::create_dir_all(&dest_dir).expect("mk dest");

        // Basis and source share size + content, but the source claims a newer
        // mtime. Under --size-only the quick-check passes on size alone, so this
        // is a match_level 2 (content matches, attrs differ) case.
        let ref_path = write_basis(&ref_dir, "payload.bin", b"hello", 0o644, MTIME);
        let entry = source_entry("payload.bin", 5, 0o644, MTIME + 10_000);

        let refs = [ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: ref_dir.clone(),
        }];

        let handled = run(&entry, &dest_dir, &refs, true, true);
        let dest_path = dest_dir.join("payload.bin");

        assert!(handled, "a matching basis must be handled locally");
        assert!(dest_path.exists(), "level-2 link-dest must copy into dest");
        assert_eq!(fs::read(&dest_path).expect("dest"), b"hello");
        assert_eq!(
            nlink(&ref_path),
            1,
            "reference inode must NOT be hard-linked at level 2 (no shared inode)"
        );
        assert_ne!(
            ino(&dest_path),
            ino(&ref_path),
            "level-2 dest must be an independent inode, not the reference's"
        );
    }

    /// (b) `--link-dest` at match_level 3 (fully identical: content, perms, and
    /// mtime all match) must hard-link. The reference inode's link count rises to
    /// 2 and the destination shares its inode - the space-saving contract of
    /// `--link-dest`.
    #[test]
    fn link_dest_level3_hard_links_shared_inode() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ref_dir = tmp.path().join("link");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir_all(&ref_dir).expect("mk ref");
        fs::create_dir_all(&dest_dir).expect("mk dest");

        let ref_path = write_basis(&ref_dir, "payload.bin", b"world", 0o644, MTIME);
        let entry = source_entry("payload.bin", 5, 0o644, MTIME);

        let refs = [ReferenceDirectory {
            kind: ReferenceDirectoryKind::Link,
            path: ref_dir.clone(),
        }];

        let handled = run(&entry, &dest_dir, &refs, true, false);
        let dest_path = dest_dir.join("payload.bin");

        assert!(handled, "identical basis must be handled locally");
        assert_eq!(
            nlink(&ref_path),
            2,
            "level-3 link-dest must hard-link (reference link count == 2)"
        );
        assert_eq!(
            ino(&dest_path),
            ino(&ref_path),
            "hard-linked dest must share the reference inode"
        );
    }

    /// (c) `--compare-dest` at match_level 2 (content matches, perms differ) must
    /// COPY the basis into the destination so the file is PRESENT there with the
    /// source's attributes. Skipping (as a naive "any match" would) leaves the
    /// destination missing a file the user expects to receive.
    #[test]
    fn compare_dest_level2_copies_into_destination() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ref_dir = tmp.path().join("compare");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir_all(&ref_dir).expect("mk ref");
        fs::create_dir_all(&dest_dir).expect("mk dest");

        // Same content + mtime (quick-check passes) but a differing mode makes
        // this match_level 2.
        write_basis(&ref_dir, "payload.bin", b"abc", 0o600, MTIME);
        let entry = source_entry("payload.bin", 3, 0o644, MTIME);

        let refs = [ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref_dir.clone(),
        }];

        let handled = run(&entry, &dest_dir, &refs, true, false);
        let dest_path = dest_dir.join("payload.bin");

        assert!(handled, "level-2 compare-dest must handle the file locally");
        assert!(
            dest_path.exists(),
            "level-2 compare-dest must copy the file into the destination"
        );
        assert_eq!(fs::read(&dest_path).expect("dest"), b"abc");
    }

    /// (d) `--compare-dest` at match_level 3 (fully identical) must SKIP: the
    /// file is already correct in the reference tree, so upstream writes nothing
    /// to the destination. The destination file must be absent.
    #[test]
    fn compare_dest_level3_skips_and_leaves_destination_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let ref_dir = tmp.path().join("compare");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir_all(&ref_dir).expect("mk ref");
        fs::create_dir_all(&dest_dir).expect("mk dest");

        write_basis(&ref_dir, "payload.bin", b"abc", 0o644, MTIME);
        let entry = source_entry("payload.bin", 3, 0o644, MTIME);

        let refs = [ReferenceDirectory {
            kind: ReferenceDirectoryKind::Compare,
            path: ref_dir.clone(),
        }];

        let handled = run(&entry, &dest_dir, &refs, true, false);
        let dest_path = dest_dir.join("payload.bin");

        assert!(
            handled,
            "an up-to-date compare-dest match must suppress the transfer"
        );
        assert!(
            !dest_path.exists(),
            "level-3 compare-dest must NOT create the destination file"
        );
    }

    /// (e) With two `--link-dest` directories where the EARLIER one is only a
    /// quick-check match (level 2, perms differ) and the LATER one is fully
    /// identical (level 3), upstream's `best_match` scan keeps the strongest
    /// match and picks the later, level-3 directory. The later reference is
    /// hard-linked (link count 2) while the earlier reference is untouched
    /// (link count 1).
    #[test]
    fn best_match_prefers_later_level3_over_earlier_level2() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let early = tmp.path().join("early");
        let late = tmp.path().join("late");
        let dest_dir = tmp.path().join("dest");
        fs::create_dir_all(&early).expect("mk early");
        fs::create_dir_all(&late).expect("mk late");
        fs::create_dir_all(&dest_dir).expect("mk dest");

        // Earlier dir: content matches but perms differ -> level 2.
        let early_path = write_basis(&early, "payload.bin", b"same", 0o600, MTIME);
        // Later dir: content, perms, and mtime all match -> level 3.
        let late_path = write_basis(&late, "payload.bin", b"same", 0o644, MTIME);
        let entry = source_entry("payload.bin", 4, 0o644, MTIME);

        let refs = [
            ReferenceDirectory {
                kind: ReferenceDirectoryKind::Link,
                path: early.clone(),
            },
            ReferenceDirectory {
                kind: ReferenceDirectoryKind::Link,
                path: late.clone(),
            },
        ];

        let handled = run(&entry, &dest_dir, &refs, true, false);
        let dest_path = dest_dir.join("payload.bin");

        assert!(handled, "a level-3 basis in a later dir must be handled");
        assert_eq!(
            nlink(&late_path),
            2,
            "the later level-3 reference must be the one hard-linked"
        );
        assert_eq!(
            ino(&dest_path),
            ino(&late_path),
            "dest must share the later (level-3) reference inode"
        );
        assert_eq!(
            nlink(&early_path),
            1,
            "the earlier level-2 reference must be left untouched"
        );
    }
}
