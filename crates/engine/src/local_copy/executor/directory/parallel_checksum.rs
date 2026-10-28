//! Parallel `--checksum` change detection for local copies.
//!
//! upstream: `checksum.c:file_checksum()` hashes the source in the sender
//! (`flist.c:make_file()`) and the destination in the generator
//! (`generator.c:quick_check_ok()`), one whole file at a time. A local copy
//! holds both sides in one process and never sends the sum anywhere, so it
//! compares them through the shared [`checksums::parallel`] service instead:
//! in parallel across files, and for large files across fixed blocks. Two
//! files are judged identical exactly when their block vectors are equal,
//! which is the same decision a whole-file compare makes.
//!
//! Every directory's same-size pairs are hashed in one batch before the
//! sequential copy loop, which then reads each decision from a
//! [`ChecksumCache`].

use std::collections::HashMap;
use std::io;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use checksums::parallel::{
    BlockDigests, FileHasher, FileJob, MAX_FILE_DIGEST_LEN, hash_files_blocked,
};
use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};

use crate::signature::SignatureAlgorithm;

/// Block length for local change detection; files of at least two blocks are
/// hashed block-parallel.
///
/// Measured on 4 pinned cores: one 2 GiB file compares in 4.3 s as 4 MiB MD5
/// blocks against 7.8 s whole-file (source and destination in parallel), and
/// 4 MiB was the best or tied-best size on every tree measured.
const LOCAL_CHECKSUM_BLOCK_LEN: NonZeroU64 = NonZeroU64::new(4 << 20).unwrap();

/// A pair of files to compare checksums.
#[derive(Debug, Clone)]
pub(crate) struct FilePair {
    /// Source file path.
    pub(crate) source: PathBuf,
    /// Destination file path.
    pub(crate) destination: PathBuf,
    /// Expected source size (for early filtering).
    pub(crate) source_size: u64,
    /// Expected destination size (for early filtering).
    pub(crate) destination_size: u64,
}

/// Streaming digest for the local copy's checksum algorithm.
enum LocalFileHasher {
    Md4(Md4),
    /// MD4 with the seed appended after the data when non-zero
    /// (upstream: checksum.c:394-395).
    Md4Seeded(Md4, i32),
    Md5(Md5),
    Sha1(Sha1),
    Xxh64(Xxh64),
    Xxh3(Xxh3),
    Xxh3_128(Xxh3_128),
}

impl LocalFileHasher {
    fn new(algorithm: SignatureAlgorithm) -> Self {
        match algorithm {
            SignatureAlgorithm::Md4 => Self::Md4(Md4::new()),
            SignatureAlgorithm::Md4Seeded { seed } => Self::Md4Seeded(Md4::new(), seed),
            SignatureAlgorithm::Md5 { seed_config } => Self::Md5(Md5::with_seed(seed_config)),
            SignatureAlgorithm::Sha1 => Self::Sha1(Sha1::new()),
            SignatureAlgorithm::Xxh64 { seed } => Self::Xxh64(Xxh64::new(seed)),
            SignatureAlgorithm::Xxh3 { seed } => Self::Xxh3(Xxh3::new(seed)),
            SignatureAlgorithm::Xxh3_128 { seed } => Self::Xxh3_128(Xxh3_128::new(seed)),
        }
    }
}

impl FileHasher for LocalFileHasher {
    fn update(&mut self, data: &[u8]) {
        match self {
            Self::Md4(h) | Self::Md4Seeded(h, _) => StrongDigest::update(h, data),
            Self::Md5(h) => StrongDigest::update(h, data),
            Self::Sha1(h) => StrongDigest::update(h, data),
            Self::Xxh64(h) => StrongDigest::update(h, data),
            Self::Xxh3(h) => StrongDigest::update(h, data),
            Self::Xxh3_128(h) => StrongDigest::update(h, data),
        }
    }

    fn finalize_into(self, out: &mut [u8; MAX_FILE_DIGEST_LEN]) -> usize {
        match self {
            Self::Md4(h) => FileHasher::finalize_into(h, out),
            Self::Md4Seeded(mut h, seed) => {
                if seed != 0 {
                    StrongDigest::update(&mut h, &seed.to_le_bytes());
                }
                FileHasher::finalize_into(h, out)
            }
            Self::Md5(h) => FileHasher::finalize_into(h, out),
            Self::Sha1(h) => FileHasher::finalize_into(h, out),
            Self::Xxh64(h) => FileHasher::finalize_into(h, out),
            Self::Xxh3(h) => FileHasher::finalize_into(h, out),
            Self::Xxh3_128(h) => FileHasher::finalize_into(h, out),
        }
    }
}

/// Two hashed sides are identical only when both were read in full and their
/// block vectors are equal; an unreadable side never matches.
fn sides_match(source: &io::Result<BlockDigests>, destination: &io::Result<BlockDigests>) -> bool {
    matches!((source, destination), (Ok(a), Ok(b)) if a == b)
}

/// Decides, for every pair, whether source and destination hold the same
/// bytes, hashing all same-size pairs in one parallel batch.
///
/// Pairs whose sizes differ are recorded as changed without being read.
fn prefetch_checksums(pairs: &[FilePair], algorithm: SignatureAlgorithm) -> HashMap<PathBuf, bool> {
    let (same_size, resized): (Vec<&FilePair>, Vec<&FilePair>) = pairs
        .iter()
        .partition(|pair| pair.source_size == pair.destination_size);
    let jobs: Vec<FileJob<'_>> = same_size
        .iter()
        .flat_map(|pair| {
            [
                FileJob {
                    path: &pair.source,
                    len: pair.source_size,
                },
                FileJob {
                    path: &pair.destination,
                    len: pair.destination_size,
                },
            ]
        })
        .collect();
    let digests = hash_files_blocked(&jobs, LOCAL_CHECKSUM_BLOCK_LEN, || {
        LocalFileHasher::new(algorithm)
    });

    let mut decisions = HashMap::with_capacity(pairs.len());
    for (pair, sides) in same_size.iter().zip(digests.chunks_exact(2)) {
        decisions.insert(pair.source.clone(), sides_match(&sides[0], &sides[1]));
    }
    for pair in resized {
        decisions.insert(pair.source.clone(), false);
    }
    decisions
}

/// Reports whether two local files of `len` bytes hold the same content,
/// hashing both sides in parallel.
///
/// Used for pairs no directory batch covered, such as single-file operands.
pub(crate) fn pair_checksums_match(
    source: &Path,
    destination: &Path,
    len: u64,
    algorithm: SignatureAlgorithm,
) -> bool {
    let jobs = [
        FileJob { path: source, len },
        FileJob {
            path: destination,
            len,
        },
    ];
    let digests = hash_files_blocked(&jobs, LOCAL_CHECKSUM_BLOCK_LEN, || {
        LocalFileHasher::new(algorithm)
    });
    sides_match(&digests[0], &digests[1])
}

/// Checksum decisions for one directory, computed in parallel before its
/// sequential copy loop.
#[derive(Debug, Default)]
pub(crate) struct ChecksumCache {
    inner: HashMap<PathBuf, bool>,
}

impl ChecksumCache {
    /// Creates a new empty checksum cache.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Decides every pair in one parallel batch.
    pub(crate) fn from_prefetch(pairs: &[FilePair], algorithm: SignatureAlgorithm) -> Self {
        Self {
            inner: prefetch_checksums(pairs, algorithm),
        }
    }

    /// Returns `Some(true)` when the source matched its destination,
    /// `Some(false)` when it differed or could not be read, and `None` when
    /// the path was not in the batch.
    pub(crate) fn lookup(&self, source: &Path) -> Option<bool> {
        self.inner.get(source).copied()
    }

    /// Returns true if the cache is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Clears all entries from the cache.
    pub(crate) fn clear(&mut self) {
        self.inner.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use test_support::create_tempdir;

    const BLOCK: usize = 4 << 20;

    fn md5() -> SignatureAlgorithm {
        SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        }
    }

    fn pair(dir: &Path, name: &str, source: &[u8], destination: &[u8]) -> FilePair {
        let src = dir.join(format!("{name}.src"));
        let dst = dir.join(format!("{name}.dst"));
        fs::write(&src, source).unwrap();
        fs::write(&dst, destination).unwrap();
        FilePair {
            source: src,
            destination: dst,
            source_size: source.len() as u64,
            destination_size: destination.len() as u64,
        }
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Identical pairs match and a one-byte change anywhere does not, for
    /// small files and for block-split files with the change in the first,
    /// a middle and the last block - so `-c` sees every change it would see
    /// comparing whole files.
    #[test]
    fn decisions_follow_content_in_every_block() {
        let dir = create_tempdir();
        let len = 2 * BLOCK + 4096;
        let base = pattern(len);
        let mut pairs = vec![
            pair(dir.path(), "small_same", b"same bytes", b"same bytes"),
            pair(dir.path(), "small_diff", b"same bytes", b"same bytez"),
            pair(dir.path(), "large_same", &base, &base),
        ];
        let mut changed = Vec::new();
        for (name, offset) in [("first", 7), ("middle", BLOCK + 9), ("last", len - 1)] {
            let mut bytes = base.clone();
            bytes[offset] ^= 0x40;
            pairs.push(pair(dir.path(), name, &base, &bytes));
            changed.push(pairs.len() - 1);
        }

        let cache = ChecksumCache::from_prefetch(&pairs, md5());
        assert_eq!(cache.lookup(&pairs[0].source), Some(true));
        assert_eq!(cache.lookup(&pairs[1].source), Some(false));
        assert_eq!(cache.lookup(&pairs[2].source), Some(true));
        for index in changed {
            assert_eq!(
                cache.lookup(&pairs[index].source),
                Some(false),
                "{}",
                pairs[index].source.display()
            );
        }
    }

    /// A size mismatch is a change without reading either file, and a
    /// missing destination never matches.
    #[test]
    fn resized_and_unreadable_pairs_are_changed() {
        let dir = create_tempdir();
        let resized = pair(dir.path(), "resized", b"short", b"much longer");
        let mut missing = pair(dir.path(), "missing", b"content", b"content");
        fs::remove_file(&missing.destination).unwrap();
        missing.destination = dir.path().join("nonexistent");

        let cache = ChecksumCache::from_prefetch(&[resized.clone(), missing.clone()], md5());
        assert_eq!(cache.lookup(&resized.source), Some(false));
        assert_eq!(cache.lookup(&missing.source), Some(false));
        assert_eq!(cache.lookup(Path::new("/not/in/batch")), None);
    }

    /// Every algorithm the local copy can select gives the same decisions.
    #[test]
    fn every_algorithm_decides_alike() {
        let dir = create_tempdir();
        let same = pair(dir.path(), "same", &pattern(3000), &pattern(3000));
        let mut other = pattern(3000);
        other[2999] ^= 1;
        let diff = pair(dir.path(), "diff", &pattern(3000), &other);
        for algorithm in [
            SignatureAlgorithm::Md4,
            SignatureAlgorithm::Md4Seeded { seed: 7 },
            md5(),
            SignatureAlgorithm::Sha1,
            SignatureAlgorithm::Xxh64 { seed: 1 },
            SignatureAlgorithm::Xxh3 { seed: 1 },
            SignatureAlgorithm::Xxh3_128 { seed: 1 },
        ] {
            assert!(pair_checksums_match(
                &same.source,
                &same.destination,
                3000,
                algorithm
            ));
            assert!(!pair_checksums_match(
                &diff.source,
                &diff.destination,
                3000,
                algorithm
            ));
        }
    }

    /// The seeded MD4 variant appends the seed after the data
    /// (upstream: checksum.c:394-395), so its digest differs from plain MD4.
    #[test]
    fn seeded_md4_appends_the_seed() {
        let mut seeded = LocalFileHasher::new(SignatureAlgorithm::Md4Seeded { seed: 9 });
        seeded.update(b"data");
        let mut out = [0u8; MAX_FILE_DIGEST_LEN];
        let len = seeded.finalize_into(&mut out);
        let mut expected = Md4::new();
        StrongDigest::update(&mut expected, b"data");
        StrongDigest::update(&mut expected, &9i32.to_le_bytes());
        assert_eq!(&out[..len], expected.finalize().as_ref());
    }
}
