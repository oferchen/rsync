#![allow(unsafe_code)]

//! Shared helpers, constants, and RAII wrappers used by the Windows ACL
//! submodules.

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::sync::Once;

use fast_io::to_extended_path;
use windows::Win32::Foundation::{ERROR_NOT_SUPPORTED, HLOCAL, LocalFree, WIN32_ERROR};
use windows::Win32::Security::PSECURITY_DESCRIPTOR;
use windows::Win32::Storage::FileSystem::{
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows::core::PWSTR;

/// Permission bit corresponding to the rsync `read` bit (0x4).
pub(super) const RSYNC_PERM_READ: u8 = 0x4;
/// Permission bit corresponding to the rsync `write` bit (0x2).
pub(super) const RSYNC_PERM_WRITE: u8 = 0x2;
/// Permission bit corresponding to the rsync `execute` bit (0x1).
pub(super) const RSYNC_PERM_EXECUTE: u8 = 0x1;

/// Emits a one-time warning about partial ACL application.
///
/// Cross-platform ACL transmission is inherently lossy (POSIX UID/GID vs
/// Windows SIDs); the warning informs operators when a particular file's
/// DACL could not be applied verbatim so they can audit the destination.
pub(super) fn warn_partial_apply() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!(
            "warning: some ACL entries could not be mapped to Windows SIDs and were dropped \
             (cross-platform ACL transmission is best-effort)"
        );
    });
}

/// Emits a per-file audit record naming the ACL entries whose principal
/// could not be resolved to a Windows SID during apply.
///
/// Unlike [`warn_partial_apply`], this diagnostic is not rate-limited and
/// names each dropped principal: cross-domain transfers can lose different
/// entries on different files, and operators need a complete per-file trail
/// of exactly which entries were discarded rather than a single opaque
/// warning. Mirrors the spirit of upstream `acls.c`, which warns when an id
/// cannot be mapped to a destination account.
pub(super) fn warn_dropped_aces(path: &Path, dropped: &[String]) {
    if dropped.is_empty() {
        return;
    }
    eprintln!(
        "warning: {}: ACL entries could not be mapped to Windows SIDs and were dropped \
         ({} total): {}",
        path.display(),
        dropped.len(),
        dropped.join(", "),
    );
}

/// Converts a Rust [`Path`] to a NUL-terminated UTF-16 buffer suitable for
/// [`windows::core::PCWSTR`] arguments.
///
/// Absolute drive and UNC paths are first routed through
/// [`fast_io::to_extended_path`] so the resulting wide string carries the
/// `\\?\` extended-length prefix. Without it, `GetNamedSecurityInfoW` /
/// `SetNamedSecurityInfoW` reject inputs longer than the 260-character
/// `MAX_PATH` cap with `ERROR_PATH_NOT_FOUND`, silently breaking ACL
/// round-trips on deeply nested trees.
pub(super) fn to_wide(path: &Path) -> Vec<u16> {
    to_extended_path(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Returns `true` when the underlying error indicates the volume does not
/// support DACLs (e.g. FAT32 mounts) or the path is not addressable.
///
/// upstream: acls.c - `no_acl_syscall_error()` swallows ENOTSUP-style errors.
pub(super) fn is_unsupported(code: WIN32_ERROR) -> bool {
    // ERROR_NOT_SUPPORTED == 50, ERROR_INVALID_FUNCTION == 1, ERROR_FILE_NOT_FOUND == 2.
    matches!(code, ERROR_NOT_SUPPORTED) || code.0 == 1 || code.0 == 2
}

/// Wraps a Win32 error code into [`io::Error`] with a stable description.
pub(super) fn win32_error(action: &str, code: WIN32_ERROR) -> io::Error {
    io::Error::other(format!("{action}: Win32 error {}", code.0))
}

/// Returns `true` for [`io::Error`] values that correspond to the same
/// "volume does not serve a security descriptor" conditions handled by
/// [`is_unsupported`].
pub(super) fn io_error_is_unsupported(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code)
            if code == ERROR_NOT_SUPPORTED.0 as i32
                || code == 1
                || code == 2
    )
}

/// Maps Windows file-access mask bits to rsync 3-bit rwx permissions.
///
/// Inheritance and synchronisation flags are intentionally collapsed
/// into the rwx triplet because the rsync wire protocol cannot represent
/// them.
pub(super) fn access_mask_to_rsync_perms(mask: u32) -> u8 {
    let mut bits: u8 = 0;
    if mask & FILE_GENERIC_READ.0 == FILE_GENERIC_READ.0 {
        bits |= RSYNC_PERM_READ;
    }
    if mask & FILE_GENERIC_WRITE.0 == FILE_GENERIC_WRITE.0 {
        bits |= RSYNC_PERM_WRITE;
    }
    if mask & FILE_GENERIC_EXECUTE.0 == FILE_GENERIC_EXECUTE.0 {
        bits |= RSYNC_PERM_EXECUTE;
    }
    bits
}

/// Reverse of [`access_mask_to_rsync_perms`]: builds a Win32 access mask.
pub(super) fn rsync_perms_to_access_mask(perms: u8) -> u32 {
    let mut mask: u32 = 0;
    if perms & RSYNC_PERM_READ != 0 {
        mask |= FILE_GENERIC_READ.0;
    }
    if perms & RSYNC_PERM_WRITE != 0 {
        mask |= FILE_GENERIC_WRITE.0;
    }
    if perms & RSYNC_PERM_EXECUTE != 0 {
        mask |= FILE_GENERIC_EXECUTE.0;
    }
    mask
}

/// Holds a Win32-allocated security descriptor.
///
/// The descriptor is owned by the kernel and must be released with
/// [`LocalFree`] once we no longer need to read its DACL pointer. The
/// `Drop` impl performs the release; callers must keep the value alive
/// for the duration of any pointer dereferences derived from it.
pub(super) struct OwnedSecurityDescriptor {
    pub(super) pd: PSECURITY_DESCRIPTOR,
}

impl Drop for OwnedSecurityDescriptor {
    fn drop(&mut self) {
        if !self.pd.0.is_null() {
            // SAFETY: `pd` was allocated by `GetNamedSecurityInfoW`, which
            // documents that callers must release the buffer with
            // `LocalFree`. We never aliased the pointer outside this struct.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.pd.0)));
            }
        }
    }
}

/// Wraps a Win32-allocated `PWSTR` so it is released with [`LocalFree`]
/// when the binding goes out of scope.
///
/// `ConvertSecurityDescriptorToStringSecurityDescriptorW` allocates the
/// output string via `LocalAlloc`; callers are required to free it with
/// `LocalFree` to avoid leaking process heap.
pub(super) struct OwnedLocalWString {
    pub(super) ptr: PWSTR,
}

impl Drop for OwnedLocalWString {
    fn drop(&mut self) {
        if !self.ptr.0.is_null() {
            // SAFETY: `ptr` was allocated by
            // `ConvertSecurityDescriptorToStringSecurityDescriptorW` and
            // is documented to require release via `LocalFree`. We never
            // alias it outside this struct.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.ptr.0.cast())));
            }
        }
    }
}
