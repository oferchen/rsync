//! Whole-file and per-block digests of many files, computed on the rayon pool.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::path::Path;

use rayon::prelude::*;

use crate::strong::StrongDigest;

// upstream: rsync.h MAX_MAP_SIZE - the read window file_checksum() hashes through.
const READ_CHUNK: usize = 256 * 1024;

/// A batch holding at least this many bytes is hashed on the rayon pool.
///
/// Measured 2026-10-28 (8-core x86_64 Linux, `taskset` 4 CPUs, tmpfs, XXH128,
/// the fastest negotiated digest, so the bar is highest for it): with few
/// files the pool loses up to 32 MiB (16 x 1 MiB: 4.1 ms serial vs 6.9 ms;
/// 32 x 1 MiB: 10.9 vs 11.2 ms) and first wins at 64 MiB (64 x 1 MiB: 19.7 vs
/// 16.5 ms).
const PARALLEL_MIN_BATCH_BYTES: u64 = 64 << 20;

/// A batch of at least this many files is hashed on the rayon pool, whatever
/// its size.
///
/// Measured with [`PARALLEL_MIN_BATCH_BYTES`]: per-file open and read costs
/// dominate small files, and at 1024 files the pool saves about 60%
/// (1024 x 4 KiB: 3.7 vs 1.5 ms; 1024 x 64 KiB: 11.8 vs 4.9 ms). Every pooled
/// batch also adds rayon scheduling syscalls (`sched_yield`, `futex`, and a
/// pool start on first use; 35 to 250 per process measured), so smaller
/// batches, whose absolute saving is a few milliseconds at most, stay serial
/// and add none.
const PARALLEL_MIN_BATCH_FILES: usize = 1024;

/// Whether `jobs` is large enough for the rayon pool to pay for itself.
///
/// The one place the serial/parallel decision is made, so every checksum site
/// on the service inherits the same measured cutoff.
fn runs_on_pool(jobs: &[FileJob<'_>]) -> bool {
    jobs.len() >= PARALLEL_MIN_BATCH_FILES
        || jobs.iter().map(|job| job.len).fold(0, u64::saturating_add) >= PARALLEL_MIN_BATCH_BYTES
}

/// Largest digest a [`FileHasher`] may produce, in bytes.
///
/// Every whole-file checksum rsync negotiates fits: SHA1 is the longest at 20.
pub const MAX_FILE_DIGEST_LEN: usize = 32;

/// Streaming whole-file digest: the algorithm a checksum site plugs into the
/// service.
///
/// Implementations carry their own algorithm choice and any seed prefix, so the
/// service never knows which algorithm it runs.
pub trait FileHasher: Send {
    /// Feeds the next run of file bytes.
    fn update(&mut self, data: &[u8]);

    /// Writes the digest into `out` and returns its length.
    ///
    /// The length must not exceed [`MAX_FILE_DIGEST_LEN`].
    fn finalize_into(self, out: &mut [u8; MAX_FILE_DIGEST_LEN]) -> usize;
}

/// Every unseeded strong digest is a file hasher, as `file_checksum()` uses it.
impl<D: StrongDigest + Send> FileHasher for D {
    fn update(&mut self, data: &[u8]) {
        StrongDigest::update(self, data);
    }

    fn finalize_into(self, out: &mut [u8; MAX_FILE_DIGEST_LEN]) -> usize {
        let digest = self.finalize();
        let bytes = digest.as_ref();
        out[..bytes.len()].copy_from_slice(bytes);
        bytes.len()
    }
}

/// One file to hash: its path and the byte count to read from it.
///
/// The length is the size recorded when the file was listed, exactly as
/// upstream hashes `st_size` bytes (`checksum.c:file_checksum()`).
#[derive(Clone, Copy, Debug)]
pub struct FileJob<'a> {
    /// File to open and read.
    pub path: &'a Path,
    /// Number of bytes to hash.
    pub len: u64,
}

/// The exact digest of a whole file, as the negotiated algorithm defines it.
///
/// This is the only value the service produces that may leave the process:
/// it is what a sender puts in the file list and what a receiver compares
/// against it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileDigest {
    bytes: [u8; MAX_FILE_DIGEST_LEN],
    len: u8,
}

impl FileDigest {
    /// Returns the digest bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

/// Per-block digests of one file, for comparing two local files only.
///
/// The blocks' digests are not the whole-file digest and cannot be combined
/// into it, so this type exposes no bytes: it can only be compared with another
/// [`BlockDigests`] computed with the same block length and algorithm. That
/// keeps it off the wire and out of any output by construction.
#[derive(Debug, Eq, PartialEq)]
pub struct BlockDigests(Vec<FileDigest>);

/// Hashes each job's whole file, in parallel across files when the batch is
/// large enough for the pool to pay for itself, serially otherwise.
///
/// Results are in `jobs` order so callers consume them in file-list order.
/// A job fails when its file cannot be opened or holds fewer than `len` bytes.
pub fn hash_files<H, F>(jobs: &[FileJob<'_>], new_hasher: F) -> Vec<io::Result<FileDigest>>
where
    H: FileHasher,
    F: Fn() -> H + Sync,
{
    let hash = |job: &FileJob<'_>| hash_range(job.path, 0, job.len, new_hasher());
    if jobs.len() < 2 || !runs_on_pool(jobs) {
        return jobs.iter().map(hash).collect();
    }
    // Ordering: callers zip results with jobs by position.
    // Preserved by par_iter().map().collect() (rayon preserves index order).
    jobs.par_iter().map(hash).collect()
}

/// Hashes each job as a vector of `block_len` block digests. A batch large
/// enough for the pool is hashed in parallel across files and, for files of at
/// least two blocks' length, across the blocks of one file; a smaller batch is
/// hashed serially. A file shorter than two blocks is one block: its
/// whole-file digest.
///
/// Only for deciding whether two local files are identical: both sides of a
/// comparison must use the same `block_len` and hasher, and files of equal
/// length then split identically. Results are in `jobs` order.
pub fn hash_files_blocked<H, F>(
    jobs: &[FileJob<'_>],
    block_len: NonZeroU64,
    new_hasher: F,
) -> Vec<io::Result<BlockDigests>>
where
    H: FileHasher,
    F: Fn() -> H + Sync,
{
    let hash = |job: &FileJob<'_>, pooled| hash_blocks(job, block_len.get(), &new_hasher, pooled);
    if !runs_on_pool(jobs) {
        return jobs.iter().map(|job| hash(job, false)).collect();
    }
    // Ordering: callers zip results with jobs by position.
    // Preserved by par_iter().map().collect() (rayon preserves index order).
    jobs.par_iter().map(|job| hash(job, true)).collect()
}

fn hash_blocks<H, F>(
    job: &FileJob<'_>,
    block_len: u64,
    new_hasher: &F,
    pooled: bool,
) -> io::Result<BlockDigests>
where
    H: FileHasher,
    F: Fn() -> H + Sync,
{
    if job.len < block_len.saturating_mul(2) {
        let digest = hash_range(job.path, 0, job.len, new_hasher())?;
        return Ok(BlockDigests(vec![digest]));
    }
    let block = |index: u64| {
        let start = index * block_len;
        let len = block_len.min(job.len - start);
        hash_range(job.path, start, len, new_hasher())
    };
    let blocks = 0..job.len.div_ceil(block_len);
    let digests = if pooled {
        blocks
            .into_par_iter()
            .map(block)
            .collect::<io::Result<Vec<_>>>()
    } else {
        blocks.map(block).collect::<io::Result<Vec<_>>>()
    };
    digests.map(BlockDigests)
}

thread_local! {
    /// One read window per thread for the life of the process, so hashing
    /// many files allocates (and maps) no per-file or per-batch buffer.
    static READ_BUFFER: RefCell<Vec<u8>> = RefCell::new(vec![0; READ_CHUNK]);
}

/// Hashes `len` bytes of `path` starting at `start` with a sized read loop.
fn hash_range<H: FileHasher>(
    path: &Path,
    start: u64,
    len: u64,
    hasher: H,
) -> io::Result<FileDigest> {
    // No rayon call happens while the buffer is borrowed, so a worker never
    // re-enters this borrow.
    READ_BUFFER.with_borrow_mut(|buf| hash_range_into(path, start, len, hasher, buf))
}

fn hash_range_into<H: FileHasher>(
    path: &Path,
    start: u64,
    len: u64,
    mut hasher: H,
    buf: &mut [u8],
) -> io::Result<FileDigest> {
    let mut file = File::open(path)?;
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut remaining = len;
    while remaining > 0 {
        let take = buf
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        file.read_exact(&mut buf[..take])?;
        hasher.update(&buf[..take]);
        remaining -= take as u64;
    }
    let mut bytes = [0u8; MAX_FILE_DIGEST_LEN];
    let len = hasher.finalize_into(&mut bytes);
    assert!(
        len <= MAX_FILE_DIGEST_LEN,
        "FileHasher digest exceeds MAX_FILE_DIGEST_LEN"
    );
    Ok(FileDigest {
        bytes,
        len: len as u8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong::{Md5, Xxh3_128};
    use std::path::PathBuf;

    fn md5() -> Md5 {
        Md5::new()
    }

    fn write_files(dir: &Path, sizes: &[usize]) -> Vec<(PathBuf, Vec<u8>)> {
        sizes
            .iter()
            .enumerate()
            .map(|(i, &size)| {
                let data: Vec<u8> = (0..size).map(|b| (b * 31 + i * 7) as u8).collect();
                let path = dir.join(format!("f{i}"));
                std::fs::write(&path, &data).unwrap();
                (path, data)
            })
            .collect()
    }

    fn jobs(files: &[(PathBuf, Vec<u8>)]) -> Vec<FileJob<'_>> {
        files
            .iter()
            .map(|(path, data)| FileJob {
                path,
                len: data.len() as u64,
            })
            .collect()
    }

    /// The parallel digest of every file must equal the one-shot digest of its
    /// bytes, at sizes on both sides of the read window, so the value a sender
    /// puts on the wire is the exact upstream whole-file checksum.
    #[test]
    fn whole_file_digest_equals_one_shot_digest_at_window_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let sizes = [
            0,
            1,
            READ_CHUNK - 1,
            READ_CHUNK,
            READ_CHUNK + 1,
            3 * READ_CHUNK + 5,
        ];
        let files = write_files(dir.path(), &sizes);
        let results = hash_files(&jobs(&files), md5);
        for ((_, data), result) in files.iter().zip(results) {
            assert_eq!(result.unwrap().as_bytes(), Md5::digest(data).as_ref());
        }
    }

    /// Results come back in job order even when files finish out of order:
    /// the large first file finishes last on any pool. The batch is past the
    /// file cutoff, so it runs on the pool.
    #[test]
    fn results_follow_job_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut sizes = vec![8 * READ_CHUNK];
        sizes.extend(std::iter::repeat_n(17, PARALLEL_MIN_BATCH_FILES));
        let files = write_files(dir.path(), &sizes);
        let results = hash_files(&jobs(&files), || Xxh3_128::with_seed(0));
        for ((_, data), result) in files.iter().zip(results) {
            assert_eq!(
                result.unwrap().as_bytes(),
                Xxh3_128::digest_with_seed(0, data).as_ref()
            );
        }
    }

    /// A missing file and a file shorter than its listed length both fail that
    /// job alone; the caller decides what a failed sum means.
    #[test]
    fn unreadable_and_short_files_fail_only_their_job() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_files(dir.path(), &[10, 10]);
        let missing = dir.path().join("missing");
        let job_list = [
            FileJob {
                path: &missing,
                len: 4,
            },
            FileJob {
                path: &files[0].0,
                len: 11,
            },
            FileJob {
                path: &files[1].0,
                len: 10,
            },
        ];
        let results = hash_files(&job_list, md5);
        assert_eq!(
            results[0].as_ref().unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            results[1].as_ref().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            results[2].as_ref().unwrap().as_bytes(),
            Md5::digest(&files[1].1).as_ref()
        );
    }

    /// Hashing only the listed length ignores bytes appended after listing,
    /// as upstream reads st_size bytes.
    #[test]
    fn hashes_only_the_listed_length() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_files(dir.path(), &[100]);
        let job = [FileJob {
            path: &files[0].0,
            len: 40,
        }];
        let result = hash_files(&job, md5).remove(0).unwrap();
        assert_eq!(result.as_bytes(), Md5::digest(&files[0].1[..40]).as_ref());
    }

    /// Records whether each hash ran on a rayon worker thread.
    struct WhereHashed(Md5, std::sync::Arc<std::sync::atomic::AtomicUsize>);

    impl FileHasher for WhereHashed {
        fn update(&mut self, data: &[u8]) {
            StrongDigest::update(&mut self.0, data);
        }

        fn finalize_into(self, out: &mut [u8; MAX_FILE_DIGEST_LEN]) -> usize {
            if rayon::current_thread_index().is_some() {
                self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            FileHasher::finalize_into(self.0, out)
        }
    }

    /// Counts the jobs hashed on a pool worker, by both entry points, for
    /// files of `sizes` bytes. The files are sparse, so a batch past the byte
    /// cutoff costs no disk.
    fn pooled_hashes(sizes: &[u64]) -> (usize, usize) {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..sizes.len())
            .map(|i| dir.path().join(format!("s{i}")))
            .collect();
        for (path, &len) in paths.iter().zip(sizes) {
            std::fs::File::create(path).unwrap().set_len(len).unwrap();
        }
        let job_list: Vec<FileJob<'_>> = paths
            .iter()
            .zip(sizes)
            .map(|(path, &len)| FileJob { path, len })
            .collect();
        let whole = Arc::new(AtomicUsize::new(0));
        let blocked = Arc::new(AtomicUsize::new(0));
        for result in hash_files(&job_list, || WhereHashed(Md5::new(), Arc::clone(&whole))) {
            result.unwrap();
        }
        let block = block_len(READ_CHUNK as u64);
        for result in hash_files_blocked(&job_list, block, || {
            WhereHashed(Md5::new(), Arc::clone(&blocked))
        }) {
            result.unwrap();
        }
        (
            whole.load(Ordering::Relaxed),
            blocked.load(Ordering::Relaxed),
        )
    }

    /// A small batch never touches the pool, so it adds no rayon scheduling
    /// syscalls over a serial run: the caller thread hashes every file.
    #[test]
    fn small_batches_hash_on_the_calling_thread() {
        let chunk = READ_CHUNK as u64;
        assert_eq!(pooled_hashes(&[3 * chunk, 100, 0, 17]), (0, 0));
        assert_eq!(
            pooled_hashes(&vec![10; PARALLEL_MIN_BATCH_FILES - 1]),
            (0, 0)
        );
        let just_under = PARALLEL_MIN_BATCH_BYTES / 2;
        assert_eq!(pooled_hashes(&[just_under, just_under - 1]), (0, 0));
    }

    /// Reaching either cutoff, by file count or by bytes, moves the batch onto
    /// the pool, where large trees get their speedup.
    #[test]
    fn batches_at_either_cutoff_hash_on_the_pool() {
        let (whole, blocked) = pooled_hashes(&vec![10; PARALLEL_MIN_BATCH_FILES]);
        assert!(whole > 0 && blocked > 0, "file cutoff: {whole} / {blocked}");
        let half = PARALLEL_MIN_BATCH_BYTES / 2;
        let (whole, blocked) = pooled_hashes(&[half, half]);
        assert!(whole > 0 && blocked > 0, "byte cutoff: {whole} / {blocked}");
    }

    fn block_len(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).unwrap()
    }

    /// One flipped byte in the first, a middle or the last block changes the
    /// block vector, so local change detection still sees every change.
    #[test]
    fn block_vectors_detect_a_change_in_any_block() {
        let dir = tempfile::tempdir().unwrap();
        let len = 9 * 1024;
        let original: Vec<u8> = (0..len).map(|b| (b % 251) as u8).collect();
        let base = dir.path().join("base");
        std::fs::write(&base, &original).unwrap();
        for offset in [0, 4 * 1024 + 3, len - 1] {
            let mut changed = original.clone();
            changed[offset] ^= 1;
            let other = dir.path().join(format!("c{offset}"));
            std::fs::write(&other, &changed).unwrap();
            let job_list = [
                FileJob {
                    path: &base,
                    len: len as u64,
                },
                FileJob {
                    path: &other,
                    len: len as u64,
                },
            ];
            let mut results = hash_files_blocked(&job_list, block_len(4 * 1024), md5);
            let b = results.pop().unwrap().unwrap();
            let a = results.pop().unwrap().unwrap();
            assert_ne!(a, b, "change at offset {offset} went undetected");
        }
    }

    /// Identical files give equal block vectors, and a single-block file is
    /// its whole-file digest, so small files cost one hash as before.
    #[test]
    fn block_vectors_of_identical_files_are_equal() {
        let dir = tempfile::tempdir().unwrap();
        let files = write_files(dir.path(), &[9 * 1024, 9 * 1024, 100]);
        let mut copies = jobs(&files);
        copies[1].path = &files[0].0;
        let results = hash_files_blocked(&copies, block_len(4 * 1024), md5);
        assert_eq!(results[0].as_ref().unwrap(), results[1].as_ref().unwrap());
        assert_eq!(results[0].as_ref().unwrap().0.len(), 3);
        let small = results[2].as_ref().unwrap();
        assert_eq!(small.0.len(), 1);
        assert_eq!(small.0[0].as_bytes(), Md5::digest(&files[2].1).as_ref());
    }
}
