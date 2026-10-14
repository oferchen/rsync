//! Extended ACL apply through a held descriptor (macOS).
//!
//! `acl_set_file` re-resolves every parent component of its path, so a parent
//! flipped to a symlink between the receiver's confined pin and the ACL apply
//! redirects an attacker-chosen ACL outside the destination tree. Upstream
//! accepts that race on macOS: its only race-safe ACL primitive is Linux-only,
//! and everywhere else it falls back to the path (`rsync-3.5.1/acls.c:1315-1322`).
//! oc deliberately goes further and applies the ACL to the pinned leaf with
//! `acl_set_fd_np`, so the parent walk is never repeated.
//!
//! Each entry is encoded exactly as upstream's `sys_acl_set_info()` does
//! (`rsync-3.5.1/lib/sysacls.c:2833-2874`): an `ACL_EXTENDED_ALLOW` or
//! `ACL_EXTENDED_DENY` tag, the member UUID from `mbr_uid_to_uuid` or
//! `mbr_gid_to_uuid` as the qualifier, then the permission and flag sets.

use std::io;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::raw::{c_int, c_uchar, c_uint, c_void};
use std::ptr;

/// One extended access-control entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtendedAce {
    /// Whether `id` is a gid rather than a uid.
    pub group: bool,
    /// The uid or gid the entry grants or denies.
    pub id: u32,
    /// `true` for an allow entry, `false` for a deny entry.
    pub allow: bool,
    /// Native `acl_perm_t` bits.
    pub perms: u32,
    /// Native `acl_flag_t` bits.
    pub flags: u32,
}

#[allow(non_camel_case_types)]
type acl_t = *mut c_void;
#[allow(non_camel_case_types)]
type acl_entry_t = *mut c_void;
#[allow(non_camel_case_types)]
type acl_permset_t = *mut c_void;
#[allow(non_camel_case_types)]
type acl_flagset_t = *mut c_void;

// <sys/acl.h>
const ACL_TYPE_EXTENDED: c_uint = 0x0000_0100;
const ACL_EXTENDED_ALLOW: c_uint = 1;
const ACL_EXTENDED_DENY: c_uint = 2;

// SAFETY: these are the <sys/acl.h> and <membership.h> prototypes; libc has
// no Apple ACL bindings. They are declared here, rather than reaching the fd
// through `exacl`, because exacl 0.13 exposes no fd-based ACL API: its `Acl`
// type is private and it writes ACLs only by path, which is the parent walk
// this module exists to avoid.
unsafe extern "C" {
    fn acl_init(count: c_int) -> acl_t;
    fn acl_free(obj: *mut c_void) -> c_int;
    fn acl_create_entry(acl: *mut acl_t, entry: *mut acl_entry_t) -> c_int;
    fn acl_set_tag_type(entry: acl_entry_t, tag: c_uint) -> c_int;
    fn acl_set_qualifier(entry: acl_entry_t, qualifier: *const c_void) -> c_int;
    fn acl_get_permset(entry: acl_entry_t, permset: *mut acl_permset_t) -> c_int;
    fn acl_clear_perms(permset: acl_permset_t) -> c_int;
    fn acl_add_perm(permset: acl_permset_t, perm: c_uint) -> c_int;
    fn acl_set_permset(entry: acl_entry_t, permset: acl_permset_t) -> c_int;
    fn acl_get_flagset_np(obj: *mut c_void, flagset: *mut acl_flagset_t) -> c_int;
    fn acl_clear_flags_np(flagset: acl_flagset_t) -> c_int;
    fn acl_add_flag_np(flagset: acl_flagset_t, flag: c_uint) -> c_int;
    fn acl_set_flagset_np(obj: *mut c_void, flagset: acl_flagset_t) -> c_int;
    fn acl_set_fd_np(fd: c_int, acl: acl_t, acl_type: c_uint) -> c_int;
    // <membership.h>
    fn mbr_uid_to_uuid(uid: u32, uu: *mut c_uchar) -> c_int;
    fn mbr_gid_to_uuid(gid: u32, uu: *mut c_uchar) -> c_int;
}

/// An owned `acl_t`, released with `acl_free` on drop.
struct Acl(acl_t);

impl Drop for Acl {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from `acl_init` (possibly moved by
        // `acl_create_entry`, which updates it in place), is non-null, and is
        // freed exactly once here.
        #[allow(unsafe_code)]
        unsafe {
            acl_free(self.0);
        }
    }
}

/// Replaces the extended ACL of the file open as `fd` with `aces`.
///
/// An empty `aces` clears every extended entry, which is how upstream resets
/// a destination whose source carries no ACL. The descriptor is never
/// resolved by name, so a parent of the file renamed or swapped for a symlink
/// after `fd` was opened cannot redirect the write.
///
/// # Errors
///
/// - `EINVAL` when `aces` is too long for `acl_init`.
/// - The error of `mbr_uid_to_uuid`/`mbr_gid_to_uuid` when an id has no
///   member UUID.
/// - The `errno` of whichever `acl_*` call fails, including `acl_set_fd_np`
///   itself (`ENOTSUP` on a filesystem without extended ACLs).
pub fn set_extended_acl_fd(fd: BorrowedFd<'_>, aces: &[ExtendedAce]) -> io::Result<()> {
    let count =
        c_int::try_from(aces.len()).map_err(|_| io::Error::from_raw_os_error(libc::EINVAL))?;
    // SAFETY: `acl_init` takes a plain count and returns an owned `acl_t` or
    // null with `errno` set; it reads no caller memory.
    #[allow(unsafe_code)]
    let raw = unsafe { acl_init(count) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut acl = Acl(raw);
    for ace in aces {
        add_entry(&mut acl, ace)?;
    }
    // SAFETY: `fd` is a live descriptor borrowed for the call and `acl.0` is
    // a valid `acl_t` owned by `acl`; `acl_set_fd_np` copies the ACL and
    // retains neither.
    #[allow(unsafe_code)]
    check(unsafe { acl_set_fd_np(fd.as_raw_fd(), acl.0, ACL_TYPE_EXTENDED) })
}

/// Appends `ace` to `acl`, mirroring `sys_acl_set_info()`.
fn add_entry(acl: &mut Acl, ace: &ExtendedAce) -> io::Result<()> {
    let mut uuid = [0 as c_uchar; 16];
    // SAFETY: `uuid` is a 16-byte `uuid_t` buffer the call fills in; the id is
    // passed by value.
    #[allow(unsafe_code)]
    let rc = unsafe {
        if ace.group {
            mbr_gid_to_uuid(ace.id, uuid.as_mut_ptr())
        } else {
            mbr_uid_to_uuid(ace.id, uuid.as_mut_ptr())
        }
    };
    if rc != 0 {
        // upstream: lib/sysacls.c:2865-2866 - the mbr_* status is an errno.
        return Err(io::Error::from_raw_os_error(rc));
    }
    let tag = if ace.allow {
        ACL_EXTENDED_ALLOW
    } else {
        ACL_EXTENDED_DENY
    };
    let mut entry: acl_entry_t = ptr::null_mut();
    let mut permset: acl_permset_t = ptr::null_mut();
    let mut flagset: acl_flagset_t = ptr::null_mut();
    // SAFETY: `acl.0` is a valid `acl_t` that `acl_create_entry` may grow and
    // move, writing the new pointer back through `&mut acl.0`. `entry`,
    // `permset` and `flagset` are only used after the call that fills them
    // succeeded, all point into `acl`, and `uuid` outlives the qualifier copy.
    #[allow(unsafe_code)]
    unsafe {
        check(acl_create_entry(&mut acl.0, &mut entry))?;
        check(acl_set_tag_type(entry, tag))?;
        check(acl_set_qualifier(entry, uuid.as_ptr().cast()))?;
        check(acl_get_permset(entry, &mut permset))?;
        check(acl_clear_perms(permset))?;
        for perm in set_bits(ace.perms) {
            check(acl_add_perm(permset, perm))?;
        }
        check(acl_set_permset(entry, permset))?;
        check(acl_get_flagset_np(entry, &mut flagset))?;
        check(acl_clear_flags_np(flagset))?;
        for flag in set_bits(ace.flags) {
            check(acl_add_flag_np(flagset, flag))?;
        }
        check(acl_set_flagset_np(entry, flagset))
    }
}

/// The individual set bits of `bits`, lowest first.
fn set_bits(bits: u32) -> impl Iterator<Item = c_uint> {
    (0..u32::BITS)
        .map(|shift| 1 << shift)
        .filter(move |bit| bits & bit != 0)
}

fn check(rc: c_int) -> io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    #[test]
    fn set_bits_splits_a_mask_into_single_bits() {
        assert_eq!(
            set_bits(0b1010_0110).collect::<Vec<_>>(),
            vec![2, 4, 32, 128]
        );
        assert_eq!(set_bits(0).count(), 0);
    }

    /// An entry for the current user lands on the file the descriptor holds,
    /// and an empty list clears it again. `ls -le` is the platform's own
    /// rendering of the extended ACL, so the assertion does not depend on
    /// this module's encoding.
    #[test]
    fn an_entry_set_through_the_fd_is_visible_and_an_empty_list_clears_it() {
        let dir = test_support::create_tempdir();
        let path = dir.path().join("target");
        let file = std::fs::File::create(&path).expect("create");
        // SAFETY: `getuid` has no preconditions and cannot fail.
        #[allow(unsafe_code)]
        let uid = unsafe { libc::getuid() };
        let ace = ExtendedAce {
            group: false,
            id: uid,
            allow: true,
            perms: 1 << 1, // ACL_READ_DATA
            flags: 0,
        };
        let listing = || {
            let out = std::process::Command::new("ls")
                .arg("-le")
                .arg(&path)
                .output()
                .expect("ls -le");
            String::from_utf8(out.stdout).expect("utf8")
        };

        set_extended_acl_fd(file.as_fd(), &[ace]).expect("set ACL");
        let with_entry = listing();
        assert!(with_entry.contains("allow read"), "{with_entry}");

        set_extended_acl_fd(file.as_fd(), &[]).expect("clear ACL");
        let cleared = listing();
        assert!(!cleared.contains("allow"), "{cleared}");
    }
}
