//! Quick-check logic for determining whether files need transfer.
//!
//! Implements upstream rsync's `quick_check_ok()` algorithm comparing
//! destination stat against source file list entries, plus reference
//! directory handling and file checksum comparison.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use protocol::flist::FileEntry;

use crate::config::{ReferenceDirectory, ReferenceDirectoryKind};
use crate::delta_apply::ChecksumVerifier;

use metadata::{MetadataOptions, apply_metadata_from_file_entry};
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
    entry.flags().hlinked() && !entry.flags().hlink_first()
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
/// 5. mtime comparison
pub(super) fn quick_check_matches(
    entry: &FileEntry,
    dest_path: &Path,
    dest_meta: &fs::Metadata,
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
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
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        dest_meta.mtime() == entry.mtime()
    }
    #[cfg(not(unix))]
    {
        dest_meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(false, |d| d.as_secs() as i64 == entry.mtime())
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

/// Checks reference directories for a file that matches the source entry.
///
/// When the destination file does not exist, this function iterates through
/// configured reference directories (`--compare-dest`, `--copy-dest`,
/// `--link-dest`) and performs the appropriate action based on kind:
///
/// - `Compare`: skip transfer entirely (file is up-to-date in reference)
/// - `Link`: create a hard link from the reference file to the destination
/// - `Copy`: copy the reference file to the destination
///
/// Returns `true` if the entry was handled and should not be transferred.
///
/// # Upstream Reference
///
/// - `generator.c:942` - `try_dests_reg()` iterates `basis_dir[]`
/// - `generator.c:983` - match_level 3 with `COMPARE_DEST` returns -2 (skip)
/// - `generator.c:991` - match_level 3 with `LINK_DEST` calls `hard_link_one()`
/// - `generator.c:1021` - match_level >= 2 with `COPY_DEST` copies locally
#[allow(clippy::too_many_arguments)]
pub(super) fn try_reference_dest(
    entry: &FileEntry,
    dest_dir: &Path,
    reference_directories: &[ReferenceDirectory],
    preserve_times: bool,
    size_only: bool,
    always_checksum: Option<protocol::ChecksumAlgorithm>,
    metadata_opts: &MetadataOptions,
    metadata_errors: &mut Vec<(PathBuf, String)>,
    acl_cache: Option<&AclCache>,
) -> bool {
    if reference_directories.is_empty() {
        return false;
    }

    let relative_path = entry.path();
    for ref_dir in reference_directories {
        let ref_path = ref_dir.path.join(relative_path);
        let ref_meta = match fs::metadata(&ref_path) {
            Ok(m) if m.is_file() => m,
            _ => continue,
        };

        // upstream: generator.c:959 - quick_check_ok against reference file
        if !quick_check_matches(
            entry,
            &ref_path,
            &ref_meta,
            preserve_times,
            size_only,
            always_checksum,
        ) {
            continue;
        }

        let dest_path = dest_dir.join(relative_path);
        match ref_dir.kind {
            ReferenceDirectoryKind::Compare => {
                // upstream: generator.c:1007 - return -2 (file is up-to-date)
                return true;
            }
            ReferenceDirectoryKind::Link => {
                // upstream: generator.c:991 - hard_link_one()
                if let Some(parent) = dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if fs::hard_link(&ref_path, &dest_path).is_ok() {
                    if let Err(e) = apply_metadata_from_file_entry(&dest_path, entry, metadata_opts)
                    {
                        metadata_errors.push((dest_path.clone(), e.to_string()));
                    }
                    if let Err(e) = apply_acls_from_receiver_cache(
                        &dest_path,
                        entry,
                        acl_cache,
                        !entry.is_symlink(),
                    ) {
                        metadata_errors.push((dest_path, e.to_string()));
                    }
                    return true;
                }
            }
            ReferenceDirectoryKind::Copy => {
                // upstream: generator.c:1021 - copy_altdest_file()
                if let Some(parent) = dest_path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if fs::copy(&ref_path, &dest_path).is_ok() {
                    if let Err(e) = apply_metadata_from_file_entry(&dest_path, entry, metadata_opts)
                    {
                        metadata_errors.push((dest_path.clone(), e.to_string()));
                    }
                    if let Err(e) = apply_acls_from_receiver_cache(
                        &dest_path,
                        entry,
                        acl_cache,
                        !entry.is_symlink(),
                    ) {
                        metadata_errors.push((dest_path, e.to_string()));
                    }
                    return true;
                }
            }
        }
    }
    false
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
    let mut buf = [0u8; 64 * 1024];
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
