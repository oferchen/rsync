//! Classification of I/O errors that indicate the target filesystem does not
//! support extended ACLs.

use std::io;

/// Returns `true` when an I/O error indicates an unsupported filesystem
/// (FAT/VFAT, network mounts without ACL support, missing kernel xattrs).
///
/// Mirrors upstream's `no_acl_syscall_error()` at `lib/sysacls.c:2778-2799`,
/// which only swallows `ENOSYS`, `ENOTSUP`, and `EINVAL` (and `ENOENT` on
/// macOS for the documented directory-ACL quirk at line 2781). `EPERM` and
/// `ENOENT` on Linux are surfaced via `rsyserr(FERROR_XFER, ...)` in
/// `set_rsync_acl()` at `acls.c:994-997` so the receiver does not silently
/// drop an ACL apply that the kernel rejected, for example when a non-root
/// receiver carries an unmappable UID/GID in a named ACL entry.
pub(super) fn is_unsupported_error(e: &io::Error) -> bool {
    if matches!(
        e.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    ) {
        return true;
    }

    match e.raw_os_error() {
        Some(libc::ENOTSUP) | Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::ENODATA) => {
            return true;
        }
        // upstream: lib/sysacls.c:2780-2782 - macOS reports ENOENT for the
        // directory-ACL quirk; Linux/FreeBSD surface ENOENT to the caller.
        #[cfg(target_os = "macos")]
        Some(libc::ENOENT) => return true,
        _ => {}
    }

    let msg = e.to_string().to_lowercase();
    msg.contains("not supported")
        || msg.contains("invalid argument")
        || msg.contains("no data")
}
