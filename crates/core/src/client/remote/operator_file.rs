//! Opening the operator-supplied auxiliary files a remote session reads.
//!
//! Two client options name a file the *operator* chose rather than one a peer
//! requested, and both are read on the way to a remote server: the
//! `--files-from` list that gets forwarded to the sender, and the
//! `--early-input` payload sent ahead of the daemon handshake. Either path may
//! transit a directory an attacker can write, and a symlink planted at any
//! component redirects the read to a file the operator never named. Upstream
//! closes this with one primitive applied at every such open.
//!
//! This module is the crate's single platform seam for that primitive. It is
//! deliberately *not* hidden behind a cross-platform `fast_io` export: the walk
//! is a security control, and a silent fallback exported as if it were portable
//! would make its absence on non-Unix invisible to a reader of the call site.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:538` `open_no_attacker_symlinks()` - walk each
//!   component without following it; follow a symlink only when it is owned by
//!   uid 0 or our euid, refuse any other-uid one (`syscall.c:406`).
//! - `rsync-3.5.0/options.c:2654` - `--files-from`.
//! - `rsync-3.5.0/clientserver.c:303` - `--early-input`.

use std::fs::File;
use std::io;
use std::path::Path;

/// Opens an operator-named file, refusing a component symlink owned by a uid
/// that is neither root nor our own.
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
