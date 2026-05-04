//! File comparison and skip-decision logic for local copies.

use std::fs;
use std::io::{self, Read};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::delta::{DeltaSignatureIndex, SignatureLayoutParams, calculate_signature_layout};
use crate::local_copy::{COPY_BUFFER_SIZE, LocalCopyError};
use crate::signature::{SignatureAlgorithm, SignatureError, generate_file_signature};

use protocol::ProtocolVersion;

/// Returns `true` when `--update` should skip this file because the
/// destination is not older than the source by more than `modify_window`.
///
/// Mirrors upstream `generator.c:2502`:
/// ```c
/// if (update_only > 0 && statret == 0
///     && file->modtime - sx.st.st_mtime < modify_window)
/// ```
///
/// With `modify_window == 0` (the default), the destination must be strictly
/// newer to trigger a skip. With `modify_window > 0`, timestamps within the
/// tolerance are treated as equal, so the source must be newer by at least
/// `modify_window` for the copy to proceed.
pub(crate) fn destination_is_newer(
    source: &fs::Metadata,
    destination: &fs::Metadata,
    modify_window: Duration,
) -> bool {
    match (source.modified(), destination.modified()) {
        (Ok(src), Ok(dst)) => match src.duration_since(dst) {
            // Source is newer than destination. Skip only when the
            // difference is strictly less than the modify window.
            // upstream: (source - dest) < modify_window
            Ok(diff) => diff < modify_window,
            // Source is older than destination — always skip.
            Err(_) => true,
        },
        _ => false,
    }
}

/// Builds a delta signature index from an existing destination file.
///
/// Returns `None` for empty files or when signature generation fails
/// for non-I/O reasons. Used by the delta transfer path to compute
/// block matches against the existing content.
pub(crate) fn build_delta_signature(
    destination: &Path,
    metadata: &fs::Metadata,
    block_size_override: Option<NonZeroU32>,
) -> Result<Option<DeltaSignatureIndex>, LocalCopyError> {
    let length = metadata.len();
    if length == 0 {
        return Ok(None);
    }

    let checksum_len = NonZeroU8::new(16).expect("strong checksum length must be non-zero");
    let params = SignatureLayoutParams::new(
        length,
        block_size_override,
        ProtocolVersion::NEWEST,
        checksum_len,
    );
    let Ok(layout) = calculate_signature_layout(params) else {
        return Ok(None);
    };

    let signature = match generate_file_signature(
        fs::File::open(destination).map_err(|error| {
            LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            )
        })?,
        layout,
        SignatureAlgorithm::Md4,
    ) {
        Ok(signature) => signature,
        Err(SignatureError::Io(error)) => {
            return Err(LocalCopyError::io(
                "read existing destination",
                destination.to_path_buf(),
                error,
            ));
        }
        Err(_) => return Ok(None),
    };

    match DeltaSignatureIndex::from_signature(&signature, SignatureAlgorithm::Md4) {
        Some(index) => Ok(Some(index)),
        None => Ok(None),
    }
}

/// Parameters for deciding whether to skip copying a file.
///
/// This struct collects all the information needed to determine if a
/// destination file is already in sync with its source.
pub(crate) struct CopyComparison<'a> {
    pub(crate) source_path: &'a Path,
    pub(crate) source: &'a fs::Metadata,
    pub(crate) destination_path: &'a Path,
    pub(crate) destination: &'a fs::Metadata,
    pub(crate) size_only: bool,
    pub(crate) ignore_times: bool,
    pub(crate) checksum: bool,
    pub(crate) checksum_algorithm: SignatureAlgorithm,
    pub(crate) modify_window: Duration,
    /// Prefetched checksum match result from parallel computation.
    ///
    /// When `Some(true)`, checksums were pre-computed and match (skip copy).
    /// When `Some(false)`, checksums were pre-computed and differ (need copy).
    /// When `None`, no prefetched result available (compute on-demand).
    pub(crate) prefetched_match: Option<bool>,
}

/// Determines whether a file copy should be skipped.
///
/// Returns `true` if the destination file is already in sync with the source
/// based on the configured comparison criteria (size, time, checksum).
///
/// When `prefetched_match` is provided, it's used directly for checksum
/// comparisons instead of recomputing the checksums.
pub(crate) fn should_skip_copy(params: CopyComparison<'_>) -> bool {
    let CopyComparison {
        source_path,
        source,
        destination_path,
        destination,
        size_only,
        ignore_times,
        checksum,
        checksum_algorithm,
        modify_window,
        prefetched_match,
    } = params;
    if destination.len() != source.len() {
        return false;
    }

    if checksum {
        return prefetched_match.unwrap_or_else(|| {
            files_checksum_match(source_path, destination_path, checksum_algorithm).unwrap_or(false)
        });
    }

    // Upstream: generator.c:unchanged_file() checks size_only before ignore_times.
    // When both flags are set, size_only wins (skip if sizes match).
    if size_only {
        return true;
    }

    if ignore_times {
        return false;
    }

    match (source.modified(), destination.modified()) {
        (Ok(src), Ok(dst)) => system_time_within_window(src, dst, modify_window),
        _ => false,
    }
}

/// Returns `true` when two timestamps differ by no more than `window`.
///
/// With a zero window, only exact equality matches.
/// // upstream: generator.c:unchanged_file() - modify window comparison
pub(crate) fn system_time_within_window(a: SystemTime, b: SystemTime, window: Duration) -> bool {
    if window.is_zero() {
        return a.eq(&b);
    }

    match a.duration_since(b) {
        Ok(diff) => diff <= window,
        Err(_) => matches!(b.duration_since(a), Ok(diff) if diff <= window),
    }
}

enum LockstepCheck {
    Continue,
    Diverged,
}

fn compare_files_lockstep<F>(source: &Path, destination: &Path, mut on_chunk: F) -> io::Result<bool>
where
    F: FnMut(&[u8], &[u8]) -> LockstepCheck,
{
    let mut source_file = fs::File::open(source)?;
    let mut destination_file = fs::File::open(destination)?;
    let mut source_buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut destination_buffer = vec![0u8; COPY_BUFFER_SIZE];

    loop {
        let source_read = source_file.read(&mut source_buffer)?;
        let destination_read = destination_file.read(&mut destination_buffer)?;

        if source_read != destination_read {
            return Ok(false);
        }

        if source_read == 0 {
            break;
        }

        match on_chunk(
            &source_buffer[..source_read],
            &destination_buffer[..destination_read],
        ) {
            LockstepCheck::Continue => {}
            LockstepCheck::Diverged => return Ok(false),
        }
    }

    Ok(true)
}

/// Compares two local files for content equality.
///
/// Uses lockstep byte comparison which is both faster and more accurate than
/// hashing for local files. Upstream rsync uses checksum comparison because
/// source and destination are on different machines, but for local copies
/// we can compare bytes directly.
pub(crate) fn files_checksum_match(
    source: &Path,
    destination: &Path,
    _algorithm: SignatureAlgorithm,
) -> io::Result<bool> {
    compare_files_lockstep(source, destination, |src_chunk, dst_chunk| {
        if src_chunk == dst_chunk {
            LockstepCheck::Continue
        } else {
            LockstepCheck::Diverged
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    use filetime::{FileTime, set_file_mtime};
    use tempfile::TempDir;
    use test_support::create_tempdir;

    /// Writes source and destination files with the given content, aligns
    /// their mtimes, and returns paths and metadata for building a
    /// [`CopyComparison`].
    fn setup_matched_mtime_files(
        source_content: &[u8],
        dest_content: &[u8],
    ) -> (TempDir, PathBuf, PathBuf, fs::Metadata, fs::Metadata) {
        let temp = create_tempdir();
        let source = temp.path().join("source.txt");
        let destination = temp.path().join("dest.txt");

        fs::write(&source, source_content).expect("write source");
        fs::write(&destination, dest_content).expect("write destination");

        let source_meta = fs::metadata(&source).expect("source metadata");
        let mtime = FileTime::from_system_time(source_meta.modified().expect("source mtime"));
        set_file_mtime(&destination, mtime).expect("set destination mtime");

        let dest_meta = fs::metadata(&destination).expect("dest metadata");
        (temp, source, destination, source_meta, dest_meta)
    }

    /// Creates two files with explicit timestamps for `destination_is_newer` tests.
    fn setup_timed_files(
        source_time: FileTime,
        dest_time: FileTime,
    ) -> (TempDir, fs::Metadata, fs::Metadata) {
        let temp = create_tempdir();
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");
        fs::write(&source, b"s").expect("write");
        fs::write(&dest, b"d").expect("write");

        set_file_mtime(&source, source_time).expect("set");
        set_file_mtime(&dest, dest_time).expect("set");

        let src_meta = fs::metadata(&source).expect("meta");
        let dst_meta = fs::metadata(&dest).expect("meta");
        (temp, src_meta, dst_meta)
    }

    /// Returns the default checksum algorithm used across comparison tests.
    fn default_checksum_algorithm() -> SignatureAlgorithm {
        SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        }
    }

    #[test]
    fn build_delta_signature_honours_block_size_override() {
        let temp = create_tempdir();
        let path = temp.path().join("data.bin");
        let mut file = fs::File::create(&path).expect("create file");
        file.write_all(&vec![0u8; 16384]).expect("write data");
        drop(file);

        let metadata = fs::metadata(&path).expect("metadata");
        let override_size = NonZeroU32::new(2048).unwrap();
        let index = build_delta_signature(&path, &metadata, Some(override_size))
            .expect("signature")
            .expect("index");

        assert_eq!(index.block_length(), override_size.get() as usize);
    }

    #[test]
    fn should_skip_copy_rewrites_with_checksum_when_metadata_matches_but_content_differs() {
        let (_temp, source, destination, source_meta, dest_meta) =
            setup_matched_mtime_files(b"fresh", b"stale");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &destination,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: true,
            checksum_algorithm: default_checksum_algorithm(),
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(!should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_accepts_identical_content_with_identical_timestamps() {
        let (_temp, source, destination, source_meta, dest_meta) =
            setup_matched_mtime_files(b"fresh", b"fresh");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &destination,
            destination: &dest_meta,
            size_only: false,
            ignore_times: false,
            checksum: false,
            checksum_algorithm: default_checksum_algorithm(),
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn should_skip_copy_size_only_wins_over_ignore_times() {
        // Upstream: generator.c:unchanged_file() checks size_only before
        // ignore_times. When both are set and sizes match, size_only wins.
        let (_temp, source, destination, source_meta, dest_meta) =
            setup_matched_mtime_files(b"fresh", b"stale");

        let comparison = CopyComparison {
            source_path: &source,
            source: &source_meta,
            destination_path: &destination,
            destination: &dest_meta,
            size_only: true,
            ignore_times: true,
            checksum: false,
            checksum_algorithm: default_checksum_algorithm(),
            modify_window: Duration::ZERO,
            prefetched_match: None,
        };

        assert!(should_skip_copy(comparison));
    }

    #[test]
    fn destination_is_newer_when_dest_is_strictly_newer_no_window() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let newer = FileTime::from_unix_time(1_700_000_005, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(older, newer);

        assert!(destination_is_newer(&src_meta, &dst_meta, Duration::ZERO));
    }

    #[test]
    fn destination_is_not_newer_when_source_is_newer() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let newer = FileTime::from_unix_time(1_700_000_005, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(newer, older);

        assert!(!destination_is_newer(&src_meta, &dst_meta, Duration::ZERO));
    }

    #[test]
    fn destination_is_not_newer_when_timestamps_equal() {
        let same = FileTime::from_unix_time(1_700_000_000, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(same, same);

        assert!(!destination_is_newer(&src_meta, &dst_meta, Duration::ZERO));
    }

    #[test]
    fn destination_is_newer_when_dest_slightly_newer_within_window() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let slightly_newer = FileTime::from_unix_time(1_700_000_000, 500_000_000);
        let (_temp, src_meta, dst_meta) = setup_timed_files(older, slightly_newer);

        // upstream: source - dest < 0 < window -> skip (dest is genuinely newer)
        assert!(destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn destination_is_newer_when_dest_far_newer_with_window() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let much_newer = FileTime::from_unix_time(1_700_000_005, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(older, much_newer);

        // upstream: source - dest = -5 < 1 -> skip (dest is newer)
        assert!(destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn destination_is_newer_at_exact_window_boundary_dest_ahead() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let exactly_at_boundary = FileTime::from_unix_time(1_700_000_002, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(older, exactly_at_boundary);

        // upstream: source - dest = -2 < 2 -> skip (dest is newer)
        assert!(destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn destination_is_newer_when_equal_with_nonzero_window() {
        let same = FileTime::from_unix_time(1_700_000_000, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(same, same);

        // upstream: source - dest = 0 < 2 -> skip (within window, equal)
        assert!(destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn destination_is_newer_when_source_slightly_newer_within_window() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let slightly_newer = FileTime::from_unix_time(1_700_000_001, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(slightly_newer, older);

        // upstream: source - dest = 1 < 2 -> skip (within window)
        assert!(destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn destination_is_not_newer_when_source_at_exact_window_boundary() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let exactly_at_boundary = FileTime::from_unix_time(1_700_000_002, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(exactly_at_boundary, older);

        // upstream: source - dest = 2, NOT < 2 -> don't skip (source definitively newer)
        assert!(!destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(2)
        ));
    }

    #[test]
    fn destination_is_not_newer_when_source_beyond_window() {
        let older = FileTime::from_unix_time(1_700_000_000, 0);
        let much_newer = FileTime::from_unix_time(1_700_000_005, 0);
        let (_temp, src_meta, dst_meta) = setup_timed_files(much_newer, older);

        // upstream: source - dest = 5, NOT < 2 -> don't skip (source definitively newer)
        assert!(!destination_is_newer(
            &src_meta,
            &dst_meta,
            Duration::from_secs(2)
        ));
    }
}
