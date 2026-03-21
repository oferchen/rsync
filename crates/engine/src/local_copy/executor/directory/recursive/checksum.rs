/// Parallel checksum prefetching for directory entries.
use std::fs;
use std::path::Path;

use crate::local_copy::CopyContext;

use super::super::parallel_checksum::{ChecksumCache, FilePair};
use super::super::planner::{DirectoryPlan, EntryAction};

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
pub(crate) fn collect_file_pairs_for_checksum(
    plan: &DirectoryPlan<'_>,
    destination: &Path,
) -> Vec<FilePair> {
    let mut pairs = Vec::new();

    for planned in &plan.planned_entries {
        if !matches!(planned.action, EntryAction::CopyFile) {
            continue;
        }

        let source_path = &planned.entry.path;
        let target_path = destination.join(Path::new(&planned.entry.file_name));
        let source_size = planned.metadata().len();

        // Check if destination exists and get its size
        let destination_size = match fs::metadata(&target_path) {
            Ok(meta) if meta.file_type().is_file() => meta.len(),
            _ => continue, // Skip if destination doesn't exist or isn't a file
        };

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

    pairs
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
    context: &CopyContext,
    plan: &DirectoryPlan<'_>,
    destination: &Path,
) -> ChecksumCache {
    // Only prefetch if checksum comparison is enabled
    if !context.checksum_enabled() {
        return ChecksumCache::new();
    }

    let pairs = collect_file_pairs_for_checksum(plan, destination);

    // Skip prefetching if no eligible pairs
    if pairs.is_empty() {
        return ChecksumCache::new();
    }

    // Compute checksums in parallel
    let algorithm = context.options().checksum_algorithm();
    ChecksumCache::from_prefetch(&pairs, algorithm)
}
