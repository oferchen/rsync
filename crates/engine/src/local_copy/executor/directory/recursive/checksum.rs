//! Parallel checksum prefetching for directory entries.
//!
//! When `--checksum` mode is active, pre-computes whole-file checksums for
//! source/destination pairs in parallel using rayon, populating a cache
//! that the per-file comparison step consults.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::CopyContext;

use super::super::super::transcode_filename_component;
use super::super::parallel_checksum::{ChecksumCache, FilePair};
use super::super::planner::{DirectoryPlan, EntryAction};
use protocol::iconv::FilenameConverter;

/// Returns `true` when the destination has exactly one hard link, so its
/// `lstat` cannot be mutated through a shared inode by an earlier sibling in
/// the same directory pass and is therefore safe to cache and reuse.
#[cfg(unix)]
fn destination_link_count_is_one(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() == 1
}

/// Non-Unix platforms expose no portable link count through `std`. Skip the
/// reuse optimization entirely so the per-file copy always performs its own
/// fresh `lstat`, matching the pre-optimization behaviour.
#[cfg(not(unix))]
fn destination_link_count_is_one(_metadata: &fs::Metadata) -> bool {
    false
}

/// Collects file pairs for parallel checksum prefetching.
///
/// This function extracts source-destination file pairs from the directory plan
/// that are candidates for checksum comparison. Only files where both source and
/// destination exist with matching sizes are included, as size mismatches already
/// indicate the files differ.
///
/// # Arguments
///
/// * `plan` - The directory plan containing planned entries
/// * `destination` - The destination directory path
///
/// # Returns
///
/// A vector of file pairs suitable for parallel checksum computation.
/// Collects source/destination file pairs from the directory plan whose
/// matching size makes them candidates for a checksum-based quick check.
///
/// When `converter` is `Some`, the destination filename is transcoded
/// from LOCAL to REMOTE so the lookup hits the same on-disk path the
/// executor will later write to. With `None` this is a zero-overhead
/// pass-through that mirrors the pre-iconv behaviour.
///
/// Alongside the pairs this returns a map of destination path to its `lstat`
/// metadata for every regular-file destination inspected. The per-file copy
/// step consumes that map so `copy_file` reuses this single `lstat` instead of
/// issuing a second one, matching upstream's one generator `link_stat` per
/// destination (upstream: generator.c:recv_generator()).
pub(crate) fn collect_file_pairs_for_checksum(
    plan: &DirectoryPlan<'_>,
    destination: &Path,
    converter: Option<&FilenameConverter>,
) -> (Vec<FilePair>, HashMap<PathBuf, fs::Metadata>) {
    let mut pairs = Vec::new();
    let mut destination_metadata = HashMap::new();

    for planned in &plan.planned_entries {
        if !matches!(planned.action, EntryAction::CopyFile) {
            continue;
        }

        let source_path = &planned.entry.path;
        let dest_name = transcode_filename_component(&planned.entry.file_name, converter);
        let target_path = destination.join(Path::new(&*dest_name));
        let source_size = planned.metadata().len();

        // upstream: generator.c:recv_generator() lstats the destination once
        // (link_stat). Use a nofollow lstat here: a symlink destination is not a
        // regular-file checksum candidate (upstream removes it and writes the
        // file), so file_type().is_file() must be false for a symlink.
        let dest_metadata = match fs::symlink_metadata(&target_path) {
            Ok(meta) if meta.file_type().is_file() => meta,
            _ => continue, // Skip if destination doesn't exist or isn't a regular file
        };
        let destination_size = dest_metadata.len();

        // Cache the lstat for copy_file to reuse only when the destination has a
        // single link. A destination hardlinked to a sibling that copy_file
        // updates earlier in this same directory pass would see its shared
        // inode's mtime change mid-loop, making this pre-pass stat stale and
        // shifting itemize output. Multi-link destinations therefore fall back
        // to copy_file's own fresh lstat, keeping itemize byte-identical.
        if destination_link_count_is_one(&dest_metadata) {
            destination_metadata.insert(target_path.clone(), dest_metadata);
        }

        // Only prefetch if sizes match (different sizes = guaranteed different content)
        if source_size == destination_size {
            pairs.push(FilePair {
                source: source_path.clone(),
                destination: target_path,
                source_size,
                destination_size,
            });
        }
    }

    (pairs, destination_metadata)
}

/// Prefetches file checksums in parallel for a directory.
///
/// When `--checksum` mode is enabled, this function computes file checksums
/// for all eligible file pairs in parallel using rayon. The results are stored
/// in a [`ChecksumCache`] that can be used during the sequential copy phase
/// to avoid recomputing checksums.
///
/// # Arguments
///
/// * `context` - The copy context (used to get checksum algorithm)
/// * `plan` - The directory plan containing files to process
/// * `destination` - The destination directory path
///
/// # Returns
///
/// A populated [`ChecksumCache`] if checksum mode is enabled and there are
/// eligible file pairs, or an empty cache otherwise.
pub(crate) fn prefetch_directory_checksums(
    context: &mut CopyContext,
    plan: &DirectoryPlan<'_>,
    destination: &Path,
) -> ChecksumCache {
    // Only prefetch if checksum comparison is enabled
    if !context.checksum_enabled() {
        return ChecksumCache::new();
    }

    let (pairs, destination_metadata) =
        collect_file_pairs_for_checksum(plan, destination, context.options().iconv());

    // Hand the destination lstat results to copy_file so it reuses them instead
    // of re-lstat'ing each destination (one generator link_stat, upstream).
    if !destination_metadata.is_empty() {
        context.set_destination_metadata_cache(destination_metadata);
    }

    // Skip prefetching if no eligible pairs
    if pairs.is_empty() {
        return ChecksumCache::new();
    }

    // Compute checksums in parallel
    let algorithm = context.options().checksum_algorithm();
    ChecksumCache::from_prefetch(&pairs, algorithm)
}
