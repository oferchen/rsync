//! Batched read-ahead of small source files for the network sender.
//!
//! A sender serving many small files pays `openat` + `fstat` + `read` + `close`
//! per file. When several whole-file requests are already buffered, their
//! sources can be opened, checked and read together: on Linux with io_uring
//! every open of a batch goes out in one `io_uring_enter(2)`, and every
//! `STATX` + `READ` + `CLOSE` chain in a second one, so the per-file syscall
//! cost collapses to roughly `2 / batch`. The kernel also runs the opens and
//! reads concurrently, which overlaps disk latency on a cold cache.
//!
//! The result is advisory. A file is returned only when the open succeeded,
//! the opened descriptor is a regular file whose size equals the expected
//! length, and the read returned exactly that many bytes - the same bytes a
//! synchronous open + `fstat` + read would have produced. Anything else
//! (error, size change, non-regular file, no io_uring) yields `None`, and the
//! caller reads that file through its normal path, which reproduces upstream's
//! exact diagnostics.
//!
//! # Upstream Reference
//!
//! - `sender.c:360-420` - the per-file open + `do_fstat` + `map_file` this
//!   batches; the size check mirrors upstream preferring `st.st_size`.

use std::path::Path;

use crate::confined_open::LeafPolicy;

/// How each prefetched source is opened.
///
/// Mirrors the sender's per-connection source-open policy so the batched open
/// resolves exactly the file the synchronous open would.
#[derive(Debug, Clone, Copy)]
pub enum PrefetchOpen<'a> {
    /// `open(path, O_RDONLY | O_CLOEXEC)`, adding `O_NOFOLLOW` when `nofollow`.
    ///
    /// upstream: `syscall.c` `do_open` / `do_open_nofollow`.
    Path {
        /// Refuse a symlinked leaf with `ELOOP`.
        nofollow: bool,
    },
    /// `openat2(root, relative, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`,
    /// the kernel arm of [`crate::open_source_confined`].
    ///
    /// Paths are given in full; the module-relative part is the path with
    /// `root` stripped. A path outside `root`, or a kernel without `openat2`,
    /// is left to the caller's synchronous open. Never batched off Linux.
    Confined {
        /// The operator-trusted module root.
        root: &'a Path,
        /// Final-component rule.
        leaf: LeafPolicy,
    },
    /// `openat2(anchor, relative, RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)`
    /// with the leaf `O_NOFOLLOW`, anchored at an already-validated root
    /// descriptor instead of re-opening `root` by path.
    ///
    /// The batched form of [`crate::SourceRoots::open`]: `root` is only
    /// stripped from each request path, never resolved.
    #[cfg(unix)]
    Anchored {
        /// The held root directory.
        anchor: std::os::fd::BorrowedFd<'a>,
        /// The root's cleaned absolute path.
        root: &'a Path,
    },
}

/// One source to prefetch: its full path and the length the file list recorded.
#[derive(Debug, Clone, Copy)]
pub struct PrefetchRequest<'a> {
    /// Full path of the source file.
    pub path: &'a Path,
    /// Expected length; the file is returned only if it still has exactly this size.
    pub len: u64,
}

/// Largest single file the batched path reads; bigger files stream normally.
pub const PREFETCH_MAX_FILE_LEN: u64 = 256 * 1024;

/// Opens, checks and reads every request, returning its contents or `None`.
///
/// The returned vector is index-aligned with `requests`. `noatime` adds
/// `O_NOATIME` to the open; a file whose open rejects it is returned as `None`
/// so the caller's open can apply its own fallback. Requests larger than
/// [`PREFETCH_MAX_FILE_LEN`] are never read here.
///
/// On platforms without io_uring, or when io_uring is unavailable at runtime,
/// every entry is `None`.
#[must_use]
pub fn prefetch_sources(
    open: PrefetchOpen<'_>,
    noatime: bool,
    requests: &[PrefetchRequest<'_>],
) -> Vec<Option<Vec<u8>>> {
    imp::prefetch_sources(open, noatime, requests)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
mod imp {
    pub(super) use crate::io_uring::source_prefetch::prefetch_sources;
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
mod imp {
    use super::{PrefetchOpen, PrefetchRequest};

    /// No io_uring on this build: every source falls back to the caller's open.
    pub(super) fn prefetch_sources(
        _open: PrefetchOpen<'_>,
        _noatime: bool,
        requests: &[PrefetchRequest<'_>],
    ) -> Vec<Option<Vec<u8>>> {
        vec![None; requests.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors [`crate::is_io_uring_available`]: without io_uring nothing is
    /// read here, so the caller's own open is the only path.
    fn batching_expected() -> bool {
        cfg!(all(target_os = "linux", feature = "io_uring")) && crate::is_io_uring_available()
    }

    fn write(dir: &Path, name: &str, data: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, data).expect("write fixture");
        path
    }

    #[test]
    fn reads_every_unchanged_file_or_falls_back() {
        let dir = test_support::create_tempdir();
        let paths: Vec<_> = (0..40)
            .map(|i| write(dir.path(), &format!("f{i}"), format!("body-{i}").as_bytes()))
            .collect();
        let empty = write(dir.path(), "empty", b"");
        let mut requests: Vec<_> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| PrefetchRequest {
                path: p,
                len: format!("body-{i}").len() as u64,
            })
            .collect();
        requests.push(PrefetchRequest {
            path: &empty,
            len: 0,
        });

        let got = prefetch_sources(PrefetchOpen::Path { nofollow: true }, false, &requests);
        assert_eq!(got.len(), requests.len());
        for (i, data) in got.iter().enumerate().take(40) {
            match data {
                Some(bytes) => assert_eq!(bytes, format!("body-{i}").as_bytes()),
                None => assert!(!batching_expected(), "file {i} must be batched"),
            }
        }
        match &got[40] {
            Some(bytes) => assert!(bytes.is_empty()),
            None => assert!(!batching_expected(), "an empty file is still batched"),
        }
    }

    /// A size change since the file list was built must not be served from
    /// the batch: upstream sends `st.st_size` bytes, which the caller's own
    /// open + fstat reproduces.
    #[test]
    fn size_mismatch_is_left_to_the_caller() {
        let dir = test_support::create_tempdir();
        let grown = write(dir.path(), "grown", b"0123456789");
        let shrunk = write(dir.path(), "shrunk", b"01");
        let requests = [
            PrefetchRequest {
                path: &grown,
                len: 4,
            },
            PrefetchRequest {
                path: &shrunk,
                len: 4,
            },
        ];
        let got = prefetch_sources(PrefetchOpen::Path { nofollow: true }, false, &requests);
        assert!(got.iter().all(Option::is_none), "{got:?}");
    }

    #[test]
    fn missing_and_oversized_files_fall_back() {
        let dir = test_support::create_tempdir();
        let missing = dir.path().join("missing");
        let big = write(
            dir.path(),
            "big",
            &vec![7u8; (PREFETCH_MAX_FILE_LEN + 1) as usize],
        );
        let requests = [
            PrefetchRequest {
                path: &missing,
                len: 3,
            },
            PrefetchRequest {
                path: &big,
                len: PREFETCH_MAX_FILE_LEN + 1,
            },
        ];
        let got = prefetch_sources(PrefetchOpen::Path { nofollow: false }, false, &requests);
        assert!(got.iter().all(Option::is_none));
    }

    /// The batched open honours `O_NOFOLLOW` exactly like `do_open_nofollow`:
    /// a leaf raced into a symlink is refused rather than read through.
    #[cfg(unix)]
    #[test]
    fn nofollow_refuses_a_symlinked_leaf() {
        let dir = test_support::create_tempdir();
        let target = write(dir.path(), "target", b"secret");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let request = [PrefetchRequest {
            path: &link,
            len: 6,
        }];
        let refused = prefetch_sources(PrefetchOpen::Path { nofollow: true }, false, &request);
        assert!(refused[0].is_none());
        let followed = prefetch_sources(PrefetchOpen::Path { nofollow: false }, false, &request);
        if batching_expected() {
            assert_eq!(followed[0].as_deref(), Some(&b"secret"[..]));
        }
    }

    /// Confined opens stay beneath the module root: an in-tree file is read,
    /// a symlink escaping the root is not.
    #[cfg(unix)]
    #[test]
    fn confined_open_stays_beneath_the_root() {
        let outside = test_support::create_tempdir();
        let secret = write(outside.path(), "secret", b"secret");
        let module = test_support::create_tempdir();
        std::fs::create_dir(module.path().join("sub")).expect("mkdir");
        let inside = write(&module.path().join("sub"), "file", b"inside");
        let escape = module.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &escape).expect("symlink");
        let escaped = escape.join("secret");
        let requests = [
            PrefetchRequest {
                path: &inside,
                len: 6,
            },
            PrefetchRequest {
                path: &escaped,
                len: 6,
            },
        ];
        let got = prefetch_sources(
            PrefetchOpen::Confined {
                root: module.path(),
                leaf: LeafPolicy::Nofollow,
            },
            false,
            &requests,
        );
        assert!(
            got[1].is_none(),
            "a path escaping the module root must not be read"
        );
        if batching_expected() && crate::linux_capabilities::openat2_supported() {
            assert_eq!(got[0].as_deref(), Some(&b"inside"[..]));
        }
        assert_eq!(std::fs::read(secret).expect("read"), b"secret");
    }
}
