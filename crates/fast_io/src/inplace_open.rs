//! The receiver's in-place output open: upstream's three-arm chain, with the
//! path resolution injected.
//!
//! Upstream opens an in-place output through a fixed sequence
//! (`receiver.c:1195-1224`):
//!
//! 1. `O_WRONLY|O_CREAT` at the target;
//! 2. on Linux, a retry without `O_CREAT` when that returned `EACCES` - the
//!    `fs.protected_regular` sysctl refuses an `O_CREAT` open of an existing
//!    file we do not own in a sticky, world-writable directory;
//! 3. on a still-`EACCES`, [`crate::readonly_inplace::open_readonly_inplace`],
//!    which grants owner-write for the duration of the open and restores the
//!    prior mode before returning.
//!
//! Arm 3 is deliberately **not** `#ifdef linux`: a read-only destination is an
//! `EACCES` everywhere.
//!
//! *Which* resolver each arm uses is upstream's second axis. `receiver.c:1204`
//! threads `one_inplace` into `secure_recv_open()` at every arm: the ordinary
//! destination leaf is opened by path, but the `--partial-dir` staging target
//! of a `one_inplace` update is an operator-supplied path walked component by
//! component through the ownership check. Threading it through *every* arm is
//! the point - a retry that dropped to the plain resolver would follow a parent
//! flipped to a symlink inside the `EACCES` window, which is exactly what
//! upstream's `partial-protected-regular-retry-linux` test plants.
//!
//! One owner for the chain, two policies: the receiver and the local-copy
//! executor live in crates that cannot share code with each other (`transfer`
//! depends on `engine`), and both need it.

use std::fs;
use std::io;
use std::path::Path;

/// How an in-place output path is resolved at every arm of the open chain.
///
/// upstream: the `one_inplace` argument to `secure_recv_open()`
/// (`receiver.c:1204-1214`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InplaceResolution {
    /// Resolve by path.
    ///
    /// The destination leaf of an ordinary `--inplace` update: it lives inside
    /// the transfer tree, whose parents the caller has already anchored.
    Direct,
    /// Walk every component, following only a symlink owned by uid 0 or our own
    /// euid.
    ///
    /// The `--partial-dir` staging target of a `one_inplace` update: an
    /// operator-supplied path that may legitimately point outside the tree, so
    /// authority - not location - is the trust signal.
    ///
    /// Unix only. `fast_io` has no ownership walk on other platforms
    /// ([`crate::owner_walk`] is `#[cfg(unix)]`), so this degrades to
    /// [`Self::Direct`] there; what path confinement should mean on Windows is
    /// an open design question, not an oversight here.
    OperatorWalk,
}

impl InplaceResolution {
    /// Open the target read-only, refusing a symlink at the leaf.
    ///
    /// The mode-restore probe of arm 3 pins an inode, so the owner-write grant
    /// and its restoration cannot be redirected between the two chmods.
    ///
    /// # Errors
    ///
    /// Propagates the open failure. Under [`Self::OperatorWalk`] that includes
    /// the walk's own refusal of a foreign-owned component.
    #[cfg(unix)]
    pub(crate) fn open_probe(self, path: &Path) -> io::Result<fs::File> {
        match self {
            Self::Direct => {
                use std::os::unix::fs::OpenOptionsExt as _;
                // upstream: receiver.c:216 - O_NOFOLLOW refuses a symlink at the
                // leaf, so the mode dance below cannot be redirected.
                fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(path)
            }
            // `operator_open_read` is `O_RDONLY` with `O_NOFOLLOW` on the leaf,
            // so it is the walked form of exactly the same open.
            Self::OperatorWalk => crate::owner_walk::operator_open_read(path),
        }
    }

    /// Open the target for writing, optionally creating and/or truncating it.
    ///
    /// # Errors
    ///
    /// Propagates the open failure. Under [`Self::OperatorWalk`] that includes
    /// the walk's own refusal of a foreign-owned component.
    pub(crate) fn open_write(
        self,
        path: &Path,
        create: bool,
        truncate: bool,
    ) -> io::Result<fs::File> {
        #[cfg(unix)]
        if self == Self::OperatorWalk {
            // upstream: receiver.c:1205 - `secure_recv_open(fnametmp,
            // O_WRONLY|O_CREAT, 0600, one_inplace)`. The final mode comes from
            // `set_file_attrs()` after the transfer, so 0600 is what the file
            // wears only while it is being written.
            return crate::owner_walk::operator_open_recv(path, create, truncate, 0o600);
        }
        fs::OpenOptions::new()
            .create(create)
            .write(true)
            .truncate(truncate)
            .open(path)
    }
}

/// Open an in-place output through upstream's three-arm chain.
///
/// `truncate` is the caller's own decision and is preserved verbatim: the
/// receiver never truncates, and the local-copy executor truncates only when
/// there is no delta basis to read back. This helper changes only *whether* the
/// open succeeds.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/receiver.c:1195-1224` - the chain in full.
///
/// # Errors
///
/// Returns the last arm's error when every arm fails.
pub fn open_inplace_output(
    path: &Path,
    truncate: bool,
    resolution: InplaceResolution,
) -> io::Result<fs::File> {
    let opened = resolution.open_write(path, true, truncate);

    // upstream: receiver.c:1211-1218 - "Maybe the error was due to
    // protected_regular setting?" Under that sysctl the kernel refuses an
    // O_CREAT open of an existing file we do not own in a sticky,
    // world-writable directory. The file exists on this path, so drop O_CREAT.
    #[cfg(target_os = "linux")]
    let opened = match opened {
        Err(error) if error.raw_os_error() == Some(libc::EACCES) => {
            resolution.open_write(path, false, truncate)
        }
        other => other,
    };

    // upstream: receiver.c:1219-1224 - the read-only-destination arm. NOT
    // platform-gated: a read-only file is an EACCES everywhere.
    #[cfg(unix)]
    let opened = match opened {
        Err(error) if error.raw_os_error() == Some(libc::EACCES) => {
            crate::readonly_inplace::open_readonly_inplace(path, truncate, resolution)
        }
        other => other,
    };

    opened
}

#[cfg(all(test, unix))]
mod tests;
