//! Read-only in-place recovery: open a mode-0444 regular file for writing by
//! granting owner-write only for as long as the open takes.
//!
//! upstream: `open_readonly_inplace()` (receiver.c:200-287), the third arm of
//! the in-place open chain at receiver.c:1219-1224 - after the primary
//! `O_WRONLY|O_CREAT` and after Linux's `protected_regular` retry.

use std::fs;
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// `CHMOD_BITS` (rsync.h) - the mode bits a chmod may carry.
const CHMOD_BITS: u32 = 0o7777;
/// `S_IWUSR`.
const OWNER_WRITE: u32 = 0o200;

/// Opens a read-only regular file for an in-place update, restoring its mode
/// before returning.
///
/// `options` supplies the caller's own open semantics (the receiver never
/// truncates; the local-copy path truncates when there is no delta basis), so
/// this owns the upstream mode dance and nothing else. It must be write-enabled.
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
pub fn open_readonly_inplace(path: &Path, options: &fs::OpenOptions) -> io::Result<fs::File> {
    let eacces = || io::Error::from_raw_os_error(libc::EACCES);

    // upstream: receiver.c:216 - O_NOFOLLOW refuses a symlink at the leaf, so
    // every syscall below acts on this one pinned inode.
    let probe = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;

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

    let opened = options.open(path);

    // upstream: receiver.c:235-241 - restore unconditionally, and a failed
    // restore WINS: drop the descriptor and report the restore error.
    match probe.set_permissions(fs::Permissions::from_mode(prior_mode)) {
        Ok(()) => opened,
        Err(restore) => Err(restore),
    }
}

#[cfg(test)]
mod tests;
