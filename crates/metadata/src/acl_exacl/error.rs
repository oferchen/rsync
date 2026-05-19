//! Classification of I/O errors that indicate the target filesystem does not
//! support extended ACLs.

use std::io;

/// Returns `true` when an I/O error indicates an unsupported filesystem
/// (FAT/VFAT, network mounts without ACL support, missing kernel xattrs).
///
/// Checks `ErrorKind`, then raw OS error codes (`ENOTSUP`, `ENOENT`, `EINVAL`,
/// `ENODATA`, `EPERM`), then common error message substrings as a last resort.
///
/// upstream: `acls.c:no_acl_syscall_error()` - errors from filesystems that
/// do not support ACLs are silently ignored.
pub(super) fn is_unsupported_error(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
    ) || {
        match e.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::ENOENT) | Some(libc::EINVAL) | Some(libc::ENODATA)
            | Some(libc::EPERM) => true,
            _ => {
                let msg = e.to_string().to_lowercase();
                msg.contains("not supported")
                    || msg.contains("no such file")
                    || msg.contains("invalid argument")
                    || msg.contains("no data")
            }
        }
    }
}
