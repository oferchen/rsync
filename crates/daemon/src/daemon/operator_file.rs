//! Reading the daemon's operator-supplied auxiliary files.
//!
//! The daemon reads several files named by the operator rather than by a peer -
//! the config file itself, any `&include`/`&merge` it pulls in, `motd file`, and
//! `secrets file`. All of them are read by a process that is typically root and
//! before (or during) the privilege drop, so a symlink planted at any component
//! of those paths redirects a privileged read to a file the operator never
//! named. Upstream closes this with one primitive applied at every such open.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:538` `open_no_attacker_symlinks()` - walk each
//!   component without following it; follow a symlink only when it is owned by
//!   uid 0 or our euid, refuse any other-uid one (`syscall.c:406`).
//! - `rsync-3.5.0/params.c:586` - the config file (and its includes).
//! - `rsync-3.5.0/clientserver.c:188` - `motd`.
//! - `rsync-3.5.0/authenticate.c:159` - `secrets file`.

use std::io;
use std::path::Path;

/// Read an operator-named daemon file, refusing an attacker-owned symlink.
///
/// Wraps [`fast_io::operator_read_to_string`] so the single platform seam lives
/// here rather than at each call site. On non-Unix targets the ownership walk
/// has no meaning (there is no `st_uid` to trust) and this degrades to a plain
/// read, matching how the rest of the tree gates `fast_io`'s operator-path
/// helpers.
pub(crate) fn read_to_string(path: &Path) -> io::Result<String> {
    #[cfg(unix)]
    {
        fast_io::operator_read_to_string(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::read_to_string(path)
    }
}
