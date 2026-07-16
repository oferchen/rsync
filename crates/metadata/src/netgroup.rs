//! Netgroup host-membership lookup for the daemon's `hosts allow`/`hosts deny`
//! `@netgroup` tokens.
//!
//! upstream: access.c `match_hostname` - when a `hosts allow`/`hosts deny`
//! token begins with `@`, rsync tests the connecting client's resolved
//! hostname for membership in the named netgroup:
//! `innetgr(tok + 1, host, NULL, NULL)` (access.c:41-42). The netgroup name is
//! `tok + 1` (the text after `@`); the second argument is the client host; the
//! user and domain arguments are `NULL`. A non-zero return means the host is a
//! member.
//!
//! The whole branch is compiled only under `HAVE_INNETGR` (access.c:40-43): on
//! platforms whose C library ships no `innetgr`/netgroup database - notably
//! musl - upstream simply omits netgroup support, so a `@netgroup` token can
//! never match. This module mirrors that exactly: where `innetgr` is available
//! (glibc, macOS, the BSDs, illumos/Solaris) it performs the real membership
//! test; everywhere else (musl, Windows, other targets) it is a no-op that
//! returns `false`. A `@netgroup` rule then never matches - graceful, not an
//! error and never a panic.

/// Targets whose C library provides `innetgr` and a netgroup database.
///
/// glibc Linux (`target_env = "gnu"`), macOS, and the BSDs / illumos / Solaris
/// ship `innetgr`. musl (`target_env = "musl"`), Android's bionic, Windows, and
/// bare targets do not, and fall through to the no-op stub below.
#[cfg(all(
    unix,
    any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "solaris",
        target_os = "illumos",
    )
))]
mod imp {
    use std::ffi::{CString, c_char, c_int};
    use std::ptr;

    // upstream: access.c innetgr @netgroup - libc does not expose `innetgr`,
    // so we declare the symbol directly. Signature per POSIX/BSD:
    // `int innetgr(const char *netgroup, const char *host,
    //              const char *user, const char *domain)`.
    #[allow(unsafe_code)]
    unsafe extern "C" {
        fn innetgr(
            netgroup: *const c_char,
            host: *const c_char,
            user: *const c_char,
            domain: *const c_char,
        ) -> c_int;
    }

    /// Real `innetgr` membership test. Interior NUL bytes make a valid C string
    /// impossible, so such input yields `false` (no match) rather than a panic.
    #[allow(unsafe_code)]
    pub(super) fn host_in_netgroup(netgroup: &str, host: &str) -> bool {
        let (Ok(netgroup), Ok(host)) = (CString::new(netgroup), CString::new(host)) else {
            return false;
        };
        // SAFETY: `netgroup` and `host` are valid NUL-terminated C strings that
        // outlive the call; `user` and `domain` are NULL, matching upstream
        // access.c:42 `innetgr(tok + 1, host, NULL, NULL)`. `innetgr` reads the
        // pointers but retains no references past the call.
        let member = unsafe { innetgr(netgroup.as_ptr(), host.as_ptr(), ptr::null(), ptr::null()) };
        member != 0
    }
}

/// Targets without `innetgr` (musl, Windows, Android, and others): a `@netgroup`
/// token can never match, mirroring an upstream build with `HAVE_INNETGR`
/// undefined.
#[cfg(not(all(
    unix,
    any(
        all(target_os = "linux", target_env = "gnu"),
        target_os = "macos",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly",
        target_os = "solaris",
        target_os = "illumos",
    )
)))]
mod imp {
    /// No-op stub: no netgroup database, so membership is always `false`.
    pub(super) fn host_in_netgroup(_netgroup: &str, _host: &str) -> bool {
        false
    }
}

/// Returns whether `host` is a member of the named `netgroup`.
///
/// Mirrors upstream `innetgr(netgroup, host, NULL, NULL)` (access.c:42): only
/// the netgroup's host field is consulted; the user and domain fields are
/// ignored. Used by the daemon to evaluate `hosts allow`/`hosts deny`
/// `@netgroup` tokens against a connecting client's resolved hostname.
///
/// On platforms without a netgroup database (musl, Windows, and others) this is
/// a no-op returning `false` - a `@netgroup` rule never matches there, exactly
/// as an upstream build compiled without `HAVE_INNETGR`. This is a graceful
/// non-match, not an error.
#[must_use]
pub fn host_in_netgroup(netgroup: &str, host: &str) -> bool {
    if netgroup.is_empty() || host.is_empty() {
        return false;
    }
    imp::host_in_netgroup(netgroup, host)
}

#[cfg(test)]
mod tests {
    use super::host_in_netgroup;

    // WHY: upstream access.c:37-38 bails out (`return 0`) before calling
    // innetgr when the host is empty; and an empty netgroup name is never a
    // real group. Guard both so callers get a deterministic non-match instead
    // of forwarding degenerate input to the C library.
    #[test]
    fn empty_inputs_never_match() {
        assert!(!host_in_netgroup("", "host.example.com"));
        assert!(!host_in_netgroup("trusted", ""));
        assert!(!host_in_netgroup("", ""));
    }

    // WHY: a netgroup that the platform cannot resolve (or, on musl/Windows,
    // netgroups being entirely unsupported) must yield a non-match, never an
    // error or panic. Using a name that cannot exist in a real netgroup db
    // exercises the "not a member" path consistently across every target.
    #[test]
    fn unknown_netgroup_yields_no_match() {
        assert!(!host_in_netgroup(
            "oc-rsync-nonexistent-netgroup-zzz",
            "host.invalid"
        ));
    }
}
