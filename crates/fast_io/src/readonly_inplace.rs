//! Read-only in-place recovery: open a mode-0444 regular file for writing by
//! granting owner-write only for as long as the open takes.
//!
//! upstream: `open_readonly_inplace()` (receiver.c:200-287), the third arm of
//! the in-place open chain at receiver.c:1219-1224 - after the primary
//! `O_WRONLY|O_CREAT` and after Linux's `protected_regular` retry.
//!
//! Reached through [`crate::inplace_open::open_inplace_output`], which owns the
//! chain; this module owns only the mode dance.

use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::inplace_open::InplaceResolution;

/// `CHMOD_BITS` (rsync.h) - the mode bits a chmod may carry.
const CHMOD_BITS: u32 = 0o7777;
/// `S_IWUSR`.
const OWNER_WRITE: u32 = 0o200;

/// Opens a read-only regular file for an in-place update, restoring its mode
/// before returning.
///
/// `truncate` is the caller's own open semantics (the receiver never truncates;
/// the local-copy path truncates when there is no delta basis), and
/// `resolution` is upstream's `one_inplace` argument - both are passed straight
/// through, so this owns the upstream mode dance and nothing else.
///
/// The prior mode is restored *before* the descriptor is handed back: once a
/// writable descriptor exists the writes no longer consult the pathname's
/// permission bits, so an abort - peer EOF, checksum failure, a signal - cannot
/// strand the file at 0600. That is what makes a cleanup path unnecessary, and
/// why the restore must never be deferred to commit time.
///
/// Upstream has two branches, fd-based (receiver.c:214) and path-based (:253).
/// This mirrors the fd-based one unconditionally: upstream keeps the path arm
/// only to retain existing pathname semantics for local and chrooted transfers,
/// and notes the fd branch is the one that pins an inode. Observable behaviour
/// is identical, and `File::set_permissions` is `fchmod` on the open descriptor,
/// so pinning costs nothing here.
///
/// # Errors
///
/// `EACCES` when the leaf is not a regular file, or is already owner-writable
/// (in which case the caller's `EACCES` came from an ACL or the parent
/// directory and a chmod could not help). Otherwise the underlying open or
/// chmod error - and a failed *restore* wins over a successful open, because
/// leaving the file owner-writable is the worse outcome.
pub fn open_readonly_inplace(
    path: &Path,
    truncate: bool,
    resolution: InplaceResolution,
) -> io::Result<fs::File> {
    let eacces = || io::Error::from_raw_os_error(libc::EACCES);

    // upstream: receiver.c:214-216 - the probe honours `one_inplace` exactly as
    // the write open does, so a raced operator path is refused here too rather
    // than silently walked by the plain resolver.
    let probe = resolution.open_probe(path)?;

    let metadata = probe.metadata()?;
    // upstream: receiver.c:219-222 - not the read-only regular file we recover.
    if !metadata.is_file() {
        return Err(eacces());
    }

    let prior_mode = metadata.permissions().mode() & CHMOD_BITS;
    // upstream: receiver.c:224-230 - each chmod risks losing a special bit, so
    // do not spend one when it cannot help.
    if prior_mode & OWNER_WRITE != 0 {
        return Err(eacces());
    }

    probe.set_permissions(fs::Permissions::from_mode(prior_mode | OWNER_WRITE))?;

    let opened = resolution.open_write(path, false, truncate);

    // upstream: receiver.c:235-241 - restore unconditionally, and a failed
    // restore WINS: drop the descriptor and report the restore error.
    match probe.set_permissions(fs::Permissions::from_mode(prior_mode)) {
        Ok(()) => opened,
        Err(restore) => Err(restore),
    }
}

#[cfg(test)]
mod tests;
