//! The parallel checksum service every `--checksum` site uses.
//!
//! upstream: `checksum.c:file_checksum()` hashes one file at a time, in the
//! sender while it builds the file list (`flist.c:make_file()`) and in the
//! generator's quick check (`generator.c:quick_check_ok()`). This module
//! computes the same digests on the rayon pool, in parallel across files, and
//! returns them in input order so callers emit and compare them in file-list
//! order. Wire bytes and output are unchanged; only wall time differs.
//!
//! Two result types keep each site on its legal path:
//!
//! - [`FileDigest`] from [`hash_files`] is the exact whole-file digest. It is
//!   the only form a value sent to or compared against a peer may take.
//! - [`BlockDigests`] from [`hash_files_blocked`] splits large files into
//!   blocks hashed in parallel. It exposes no bytes and is only comparable with
//!   another `BlockDigests`, so it serves local change detection alone.
//!
//! The algorithm is a [`FileHasher`] the caller supplies (Strategy pattern):
//! the service reads files and schedules work, the hasher defines the digest.
//! Every [`StrongDigest`](crate::strong::StrongDigest) is a `FileHasher`.
//!
//! ```
//! use checksums::parallel::{FileJob, hash_files};
//! use checksums::strong::{Md5, StrongDigest};
//!
//! let dir = tempfile::tempdir().unwrap();
//! let path = dir.path().join("file");
//! std::fs::write(&path, b"data").unwrap();
//!
//! let jobs = [FileJob { path: &path, len: 4 }];
//! let digests = hash_files(&jobs, Md5::new);
//! assert_eq!(digests[0].as_ref().unwrap().as_bytes(), Md5::digest(b"data"));
//! ```

mod files;

pub use files::{
    BlockDigests, FileDigest, FileHasher, FileJob, MAX_FILE_DIGEST_LEN, hash_files,
    hash_files_blocked,
};
