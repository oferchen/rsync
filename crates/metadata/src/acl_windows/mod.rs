#![cfg(all(feature = "acl", windows))]
#![allow(unsafe_code)]

//! Windows ACL synchronisation via Win32 `GetNamedSecurityInfoW` and
//! `SetNamedSecurityInfoW`.
//!
//! This module bridges oc-rsync's wire-protocol ACL representation
//! ([`RsyncAcl`]) to NTFS DACLs through the
//! [`windows::Win32::Security::Authorization`] FFI surface. It mirrors the
//! upstream rsync ACL flow on Windows hosts so that `--acls`/`-A`
//! preserves discretionary access control entries when both endpoints
//! support ACL semantics.
//!
//! # Scope
//!
//! - The default read path (`read_dacl_sddl`) covers the owner, group, and
//!   discretionary ACL (DACL). The system ACL (SACL) requires the
//!   `SE_SECURITY_NAME` privilege and is read only on the opt-in
//!   `read_sddl_with_sacl` path, avoiding surprising privilege escalations
//!   on standard accounts.
//! - Applied DACLs set the protected (`P`) bit so the destination does not
//!   silently inherit ACEs from its parent. SACL entries carried in an SDDL
//!   payload are written only when the calling token holds
//!   `SE_SECURITY_NAME`.
//!
//! # SID/UID Mapping
//!
//! Upstream rsync transmits ACEs by numeric uid/gid plus an optional
//! account name string. On Unix the names are looked up with
//! `getpwuid`/`getgrgid`; on Windows there is no POSIX uid/gid, so this
//! module follows a "best-effort" lossy convention:
//!
//! - **Sender:** for each translatable SID, encode the account name and
//!   use the lower sub-authority (RID) as the synthetic uid/gid.
//!   Untranslatable SIDs are dropped, matching upstream's lossy
//!   cross-platform ACL semantics (see `acls.c:902-928`).
//! - **Receiver:** look up the SID for the encoded account name. If no
//!   name was sent or the lookup fails, the ACE is dropped, again
//!   matching upstream's lossy cross-platform semantics.
//!
//! # Upstream Reference
//!
//! - `acls.c:580-668` (`send_rsync_acl`, `send_acl`)
//! - `acls.c:670-800` (`recv_rsync_acl`, `recv_acl`)
//! - `acls.c:830-1000` (`set_acl`, `change_sacl_perms`)

mod common;
mod dacl;
mod posix_map;
mod sddl;
mod sync;
mod xattr;

#[cfg(test)]
mod tests;

pub use dacl::{apply_acls_from_cache, default_perms_for_dir, get_rsync_acl};
pub use posix_map::{dacl_to_posix_mode, posix_mode_to_dacl};
pub use sddl::{read_dacl_sddl, read_sddl_with_sacl, write_dacl_sddl};
pub use sync::sync_acls;
pub use xattr::{
    WINDOWS_SDDL_XATTR_NAME, apply_sddl_from_xattrs, find_sddl_in_xattrs, sddl_xattr_entry,
};
