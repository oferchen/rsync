//! Opening the client's operator-supplied auxiliary files.
//!
//! Several options name a file the *operator* chose rather than one a peer
//! requested - `--files-from`, `--exclude-from`/`--include-from`, a `merge`
//! rule, `--password-file`. Those paths may transit directories an attacker can
//! write, and a symlink planted at any component redirects the read to a file
//! the operator never named. Upstream closes this with one primitive applied at
//! every such open.
//!
//! This module is the crate's single platform seam for that primitive, so the
//! `cfg` is written once instead of once per option. It is deliberately *not*
//! hidden behind a cross-platform `fast_io` export: the walk is a security
//! control, and a silent fallback exported as if it were portable would make
//! its absence on non-Unix invisible to a reader of the call site.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:538` `open_no_attacker_symlinks()` - walk each
//!   component without following it; follow a symlink only when it is owned by
//!   uid 0 or our euid, refuse any other-uid one (`syscall.c:406`).
//! - `rsync-3.5.0/options.c:2654` - `--files-from`.
//! - `rsync-3.5.0/exclude.c:1683` - `--*clude-from` and `merge`.
//! - `rsync-3.5.0/authenticate.c:245` - `--password-file`.

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
