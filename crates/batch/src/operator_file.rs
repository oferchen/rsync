//! Opening the operator-supplied batch files.
//!
//! `--read-batch`, `--write-batch` and the generated `.sh` companion all name a
//! file the *operator* chose. Those paths may transit directories an attacker
//! can write, and a symlink planted at any component redirects the open to a
//! file the operator never named - which for the two write sides means a
//! privileged create-and-truncate lands somewhere else entirely. Upstream
//! closes this with one primitive applied at every such open.
//!
//! This module is the crate's single platform seam for that primitive, so the
//! `cfg` is written once instead of once per call site. It is deliberately
//! *not* hidden behind a cross-platform `fast_io` export: the walk is a
//! security control, and a silent fallback exported as if it were portable
//! would make its absence on non-Unix invisible to a reader of the call site.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:538` `open_no_attacker_symlinks()` - walk each
//!   component without following it; follow a symlink only when it is owned by
//!   uid 0 or our euid, refuse any other-uid one (`syscall.c:406`).
//! - `rsync-3.5.0/batch.c:254` - the `.sh` companion, created `0700`.
//! - `rsync-3.5.0/batch.c:263` - `--write-batch`, created `0600`.
//! - `rsync-3.5.0/batch.c:267` - `--read-batch`, opened read-only.

use std::fs::File;
use std::io;
use std::path::Path;

/// Mode upstream creates the batch file with (`batch.c:263`).
///
/// Owner-only: the batch stream carries the transferred file *contents*, so a
/// world-readable batch would publish everything the transfer moved.
pub(crate) const BATCH_FILE_MODE: u32 = 0o600;

/// Mode upstream creates the `.sh` companion with (`batch.c:254`).
///
/// Owner-only plus execute, because the generated script is meant to be run.
pub(crate) const BATCH_SCRIPT_MODE: u32 = 0o700;

/// Opens an operator-named file for reading, refusing a component symlink owned
/// by a uid that is neither root nor our own.
///
/// On non-Unix targets the ownership walk has no meaning - there is no `st_uid`
/// to trust - so this degrades to a plain open, matching how the rest of the
/// tree gates `fast_io`'s operator-path helpers.
pub(crate) fn open_read(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        fast_io::operator_open_read(path)
    }
    #[cfg(not(unix))]
    {
        File::open(path)
    }
}

/// Creates or truncates an operator-named file for writing, refusing a
/// component symlink owned by a uid that is neither root nor our own.
///
/// `mode` applies only when the file is created, exactly as upstream's
/// `O_CREAT|O_TRUNC` open does - re-running `--write-batch` over an existing
/// file truncates it and leaves its mode alone.
///
/// On non-Unix targets this degrades to a plain create, as above; `mode` has no
/// counterpart there and is ignored, which is also why the caller must not rely
/// on it for anything but Unix parity.
pub(crate) fn create_write(path: &Path, mode: u32) -> io::Result<File> {
    #[cfg(unix)]
    {
        fast_io::operator_open_write_create(path, mode)
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        File::create(path)
    }
}
