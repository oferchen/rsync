//! File comparison and skip-decision logic for local copies.

use std::fs;
use std::io::{self, Read};
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;
use std::time::{Duration, SystemTime};

use checksums::strong::Xxh64;

use crate::delta::{DeltaSignatureIndex, SignatureLayoutParams, calculate_signature_layout};
use crate::local_copy::{COPY_BUFFER_SIZE, LocalCopyError};
use crate::signature::{
    PARALLEL_THRESHOLD_BYTES, SignatureAlgorithm, SignatureError, generate_file_signature,
    generate_file_signature_windowed,
};

use protocol::ProtocolVersion;

/// Default upper bound for the xxh64 dedup heuristic.
///
/// Files larger than this size are skipped because the cost of hashing both
/// sides dominates any savings from avoiding the rolling+strong checksum
/// pipeline. The value can be overridden via
/// [`LocalCopyOptions::xxh64_dedup_size_limit`](crate::local_copy::LocalCopyOptions::xxh64_dedup_size_limit).
pub(crate) const DEFAULT_XXH64_DEDUP_SIZE_LIMIT: u64 = 8 * 1024 * 1024;

/// Seed used by the xxh64 dedup heuristic.
///
/// Fixed value chosen to avoid collisions with rsync block-checksum seeds.
const XXH64_DEDUP_SEED: u64 = 0x6F632D72_73796E63; // "oc-rsync"

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
            // Source is older than destination - always skip.
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

    let basis = fs::File::open(destination).map_err(|error| {
        LocalCopyError::io(
            "read existing destination",
            destination.to_path_buf(),
            error,
        )
    })?;

    // Computing the basis block checksums (the delta signature) is the
    // single-threaded hot path of a delta transfer: perf shows md4::compress
    // plus generate_file_signature pinning one core. For a large basis, fan
    // the per-block rolling+strong checksums across the rayon pool with the
    // bounded-memory windowed generator. Its output is byte-identical to the
    // sequential path; below the threshold the rayon overhead would dominate,
    // so stay sequential there.
    let signature_result = if length >= PARALLEL_THRESHOLD_BYTES {
        generate_file_signature_windowed(basis, layout, SignatureAlgorithm::Md4)
    } else {
        generate_file_signature(basis, layout, SignatureAlgorithm::Md4)
    };
    let signature = match signature_result {
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

/// Returns the whole-second UNIX timestamp for `time`, discarding any
/// sub-second component.
///
/// Floors toward negative infinity so pre-epoch timestamps map to the same
/// second regardless of their fractional part, mirroring the `time_t`
/// truncation upstream applies before comparing modification times.
fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(diff) => diff.as_secs() as i64,
        Err(error) => {
            let diff = error.duration();
            // A non-zero sub-second remainder means the instant lands strictly
            // before the flooring boundary, so subtract one to floor.
            if diff.subsec_nanos() == 0 {
                -(diff.as_secs() as i64)
            } else {
                -(diff.as_secs() as i64) - 1
            }
        }
    }
}

/// Returns `true` when two timestamps differ by no more than `window`.
///
/// With a zero window the comparison is at whole-second granularity: the
/// sub-second component of each timestamp is discarded before comparing. This
/// mirrors upstream, which stores the source `modtime` as a `time_t` and whose
/// `same_time()` helper reduces to `f1_sec == f2_sec` when `modify_window == 0`,
/// so a destination that only differs in the fractional second is still treated
/// as up-to-date by the quick check.
///
/// With a non-zero window the original full-resolution duration comparison is
/// retained so sub-second `--modify-window` tolerances continue to apply.
///
/// upstream: generator.c:unchanged_file() -> same_time() - whole-second
/// comparison when `modify_window == 0`.
pub(crate) fn system_time_within_window(a: SystemTime, b: SystemTime, window: Duration) -> bool {
    if window.is_zero() {
        return unix_seconds(a) == unix_seconds(b);
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

/// Result of the xxh64 dedup heuristic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Xxh64DedupOutcome {
    /// The heuristic did not run because the file exceeded the configured
    /// size limit.
    Skipped,
    /// Both files produced the same xxh64 digest; treat the destination as
    /// identical to the source and bypass delta computation.
    Match,
    /// Files produced different xxh64 digests; the caller should fall through
    /// to the normal delta path.
    Differ,
}

/// Streams `file` end-to-end into an [`Xxh64`] hasher and returns the digest.
fn hash_file_xxh64(file: &mut fs::File) -> io::Result<[u8; 8]> {
    let mut hasher = Xxh64::new(XXH64_DEDUP_SEED);
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

/// Runs the internal xxh64 file-dedup heuristic.
///
/// When enabled, both `source` and `destination` are streamed through xxh64
/// and the digests are compared. A match indicates with very high probability
/// that the files are byte-identical, so the caller can bypass the
/// rolling+strong checksum pipeline.
///
/// The heuristic is purely local (no wire protocol change) and is gated by
/// the supplied size limit; files larger than `size_limit` return
/// [`Xxh64DedupOutcome::Skipped`] so the cost of hashing does not eclipse the
/// delta savings.
///
/// Returns [`Xxh64DedupOutcome::Match`] only when the source size equals the
/// destination size and the digests match.
pub(crate) fn xxh64_dedup_check(
    source: &Path,
    destination: &Path,
    source_size: u64,
    destination_size: u64,
    size_limit: u64,
) -> io::Result<Xxh64DedupOutcome> {
    if source_size != destination_size {
        return Ok(Xxh64DedupOutcome::Differ);
    }
    if source_size > size_limit {
        return Ok(Xxh64DedupOutcome::Skipped);
    }

    let mut source_file = fs::File::open(source)?;
    let mut destination_file = fs::File::open(destination)?;
    let source_digest = hash_file_xxh64(&mut source_file)?;
    let destination_digest = hash_file_xxh64(&mut destination_file)?;

    Ok(if source_digest == destination_digest {
        Xxh64DedupOutcome::Match
    } else {
        Xxh64DedupOutcome::Differ
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
        SignatureAlgorithm::Xxh3_128 { seed: 0 }
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
    fn system_time_within_zero_window_ignores_subsecond_difference() {
        // upstream: generator.c:unchanged_file() -> same_time() reduces to
        // `f1_sec == f2_sec` when modify_window == 0, so a destination written
        // without `-t` that lands in the same second as the source (but with a
        // different fractional part) is still treated as up-to-date. Without
        // this the quick check would re-transfer a content-identical file on
        // every run, which surfaced as a missing `is uptodate` notice for an
        // already-hardlinked alias under `-vvH`.
        let base = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 100_000_000);
        let same_second = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 900_000_000);
        assert!(system_time_within_window(base, same_second, Duration::ZERO));
    }

    #[test]
    fn system_time_within_zero_window_separates_whole_second_difference() {
        // A one-second difference must still be observed at zero window so a
        // genuinely newer source is not skipped.
        let earlier = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_000, 900_000_000);
        let next_second = SystemTime::UNIX_EPOCH + Duration::new(1_700_000_001, 0);
        assert!(!system_time_within_window(
            earlier,
            next_second,
            Duration::ZERO
        ));
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

    fn write_file(path: &Path, contents: &[u8]) {
        let mut file = fs::File::create(path).expect("create file");
        file.write_all(contents).expect("write contents");
    }

    #[test]
    fn xxh64_dedup_check_reports_match_for_identical_files() {
        let temp = create_tempdir();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        let payload = vec![0xABu8; 4096];
        write_file(&source, &payload);
        write_file(&destination, &payload);

        let outcome = xxh64_dedup_check(
            &source,
            &destination,
            payload.len() as u64,
            payload.len() as u64,
            DEFAULT_XXH64_DEDUP_SIZE_LIMIT,
        )
        .expect("dedup check");

        assert_eq!(outcome, Xxh64DedupOutcome::Match);
    }

    #[test]
    fn xxh64_dedup_check_reports_differ_for_distinct_content() {
        let temp = create_tempdir();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        let mut source_bytes = vec![0u8; 4096];
        for (offset, byte) in source_bytes.iter_mut().enumerate() {
            *byte = (offset & 0xFF) as u8;
        }
        let mut dest_bytes = source_bytes.clone();
        dest_bytes[2048] ^= 0xFF;
        write_file(&source, &source_bytes);
        write_file(&destination, &dest_bytes);

        let outcome = xxh64_dedup_check(
            &source,
            &destination,
            source_bytes.len() as u64,
            dest_bytes.len() as u64,
            DEFAULT_XXH64_DEDUP_SIZE_LIMIT,
        )
        .expect("dedup check");

        assert_eq!(outcome, Xxh64DedupOutcome::Differ);
    }

    #[test]
    fn xxh64_dedup_check_skips_when_file_exceeds_size_limit() {
        let temp = create_tempdir();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        let payload = vec![0xCDu8; 2048];
        write_file(&source, &payload);
        write_file(&destination, &payload);

        let outcome = xxh64_dedup_check(
            &source,
            &destination,
            payload.len() as u64,
            payload.len() as u64,
            1024,
        )
        .expect("dedup check");

        assert_eq!(outcome, Xxh64DedupOutcome::Skipped);
    }

    #[test]
    fn xxh64_dedup_check_short_circuits_when_sizes_differ() {
        let temp = create_tempdir();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("dest.bin");
        write_file(&source, &vec![0u8; 1024]);
        write_file(&destination, &vec![0u8; 512]);

        let outcome = xxh64_dedup_check(
            &source,
            &destination,
            1024,
            512,
            DEFAULT_XXH64_DEDUP_SIZE_LIMIT,
        )
        .expect("dedup check");

        assert_eq!(outcome, Xxh64DedupOutcome::Differ);
    }
}
