//! Whole-file and per-block digests of many files, computed on the rayon pool.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::path::Path;

use rayon::prelude::*;

use crate::strong::StrongDigest;

// upstream: rsync.h MAX_MAP_SIZE - the read window file_checksum() hashes through.
const READ_CHUNK: usize = 256 * 1024;

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

/// Hashes each job's whole file, in parallel across files.
///
/// Results are in `jobs` order so callers consume them in file-list order.
/// A job fails when its file cannot be opened or holds fewer than `len` bytes.
pub fn hash_files<H, F>(jobs: &[FileJob<'_>], new_hasher: F) -> Vec<io::Result<FileDigest>>
where
    H: FileHasher,
    F: Fn() -> H + Sync,
{
    let hash =
        |buf: &mut Vec<u8>, job: &FileJob<'_>| hash_range(job.path, 0, job.len, new_hasher(), buf);
    if jobs.len() < 2 {
        let mut buf = read_buffer();
        return jobs.iter().map(|job| hash(&mut buf, job)).collect();
    }
    // Ordering: callers zip results with jobs by position.
    // Preserved by par_iter().map_init().collect() (rayon preserves index order).
    jobs.par_iter().map_init(read_buffer, hash).collect()
}

/// Hashes each job as a vector of `block_len` block digests, in parallel across
/// files and, for files of at least two blocks' length, across the blocks of
/// one file. A shorter file is one block: its whole-file digest.
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
    // Ordering: callers zip results with jobs by position.
    // Preserved by par_iter().map_init().collect() (rayon preserves index order).
    jobs.par_iter()
        .map_init(read_buffer, |buf, job| {
            hash_blocks(job, block_len.get(), &new_hasher, buf)
        })
        .collect()
}

fn hash_blocks<H, F>(
    job: &FileJob<'_>,
    block_len: u64,
    new_hasher: &F,
    buf: &mut [u8],
) -> io::Result<BlockDigests>
where
    H: FileHasher,
    F: Fn() -> H + Sync,
{
    if job.len < block_len.saturating_mul(2) {
        let digest = hash_range(job.path, 0, job.len, new_hasher(), buf)?;
        return Ok(BlockDigests(vec![digest]));
    }
    (0..job.len.div_ceil(block_len))
        .into_par_iter()
        .map_init(read_buffer, |buf, index| {
            let start = index * block_len;
            let len = block_len.min(job.len - start);
            hash_range(job.path, start, len, new_hasher(), buf)
        })
        .collect::<io::Result<Vec<_>>>()
        .map(BlockDigests)
}

fn read_buffer() -> Vec<u8> {
    vec![0; READ_CHUNK]
}

/// Hashes `len` bytes of `path` starting at `start` with a sized read loop.
fn hash_range<H: FileHasher>(
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
    /// the large first file finishes last on any pool.
    #[test]
    fn results_follow_job_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut sizes = vec![8 * READ_CHUNK];
        sizes.extend(std::iter::repeat_n(17, 64));
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
