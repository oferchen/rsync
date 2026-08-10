//! `--debug=ACL` producer emissions for POSIX ACL processing.
//!
//! Matches upstream rsync's `acls.c` `DEBUG_GTE(ACL, N)` output byte-for-byte
//! so wire-comparable diagnostics align across implementations.
//!
//! # Upstream Reference
//!
//! - `acls.c:1083-1139` `default_perms_for_dir` - reads a parent directory's
//!   default ACL to derive `dest_mode()`'s `dflt_perms` argument when
//!   `--perms` is off (`generator.c:1338-1339`, `receiver.c:846-847`).
//! - `acls.c:1133-1134` (`DEBUG_GTE(ACL, 1)`) - `"got ACL-based default perms
//!   %o for directory %s\n"` - the sole upstream ACL debug emission.
//! - `options.c:290` - `DEBUG_WORD(ACL, W_SND|W_REC, "Debug extra ACL info")`
//!   flag table entry, capping the useful level at 1.

use logging::debug_log;

/// Traces ACL-derived default permission bits for a parent directory (level 1).
///
/// upstream: `acls.c:1133-1134` - `"got ACL-based default perms %o for
/// directory %s\n"`. Emitted by `default_perms_for_dir` after the directory's
/// default POSIX ACL unpacked successfully and yielded a `user_obj` entry the
/// caller can fold into `dest_mode()`. The `perms` value renders as upstream's
/// `%o` (octal, no leading zero, no width padding) and matches the permission
/// bits extracted via `rsync_acl_get_perms(&racl)`.
#[inline]
pub fn trace_default_perms_for_dir(perms: u32, dir: &str) {
    debug_log!(
        Acl,
        1,
        "got ACL-based default perms {:o} for directory {}",
        perms,
        dir
    );
}

/// Traces a UID remap for a named ACL entry (level 2).
///
/// upstream: `uidlist.c:287-291` - `"uid %u(%s) maps to %u\n"`, emitted by
/// `recv_add_id()` under `DEBUG_GTE(OWN, 2)` whenever an inbound ACL or file
/// list entry's wire UID is resolved against the local NSS database. When
/// `getpwnam_r` finds the wire name, `mapped` is the local UID; when it does
/// not, upstream falls back to `id2 = id` at `uidlist.c:282`, so the
/// emission still fires with `mapped == wire`. The receiver passes the
/// resolved UID straight to `sys_acl_set_info()` (`acls.c:404`) without
/// dropping the entry.
#[inline]
pub fn trace_acl_uid_remap(wire: u32, name: &str, mapped: u32) {
    debug_log!(Own, 2, "uid {}({}) maps to {}", wire, name, mapped);
}

/// Traces a GID remap for a named ACL entry (level 2).
///
/// upstream: `uidlist.c:287-291` - `"gid %u(%s) maps to %u\n"`, emitted by
/// `recv_add_id()` under `DEBUG_GTE(OWN, 2)` for inbound group entries.
/// Mirrors the UID variant in [`trace_acl_uid_remap`].
#[inline]
pub fn trace_acl_gid_remap(wire: u32, name: &str, mapped: u32) {
    debug_log!(Own, 2, "gid {}({}) maps to {}", wire, name, mapped);
}

#[cfg(test)]
mod tests {
    //! Pinning tests for ACL emission shapes. Strings match upstream
    //! `acls.c` byte-for-byte.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.acl = level;
        init(cfg);
        let _ = drain_events();
    }

    fn acl_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Acl,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn own_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Own,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    fn init_own_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.own = level;
        init(cfg);
        let _ = drain_events();
    }

    #[test]
    fn default_perms_wire_shape() {
        // upstream: acls.c:1133-1134 - "got ACL-based default perms %o for directory %s"
        init_at(1);
        trace_default_perms_for_dir(0o755, "/tmp/dst");
        trace_default_perms_for_dir(0o700, ".");
        trace_default_perms_for_dir(0o644, "nested/sub");

        let m = acl_messages();
        for expected in [
            "got ACL-based default perms 755 for directory /tmp/dst",
            "got ACL-based default perms 700 for directory .",
            "got ACL-based default perms 644 for directory nested/sub",
        ] {
            assert!(m.iter().any(|s| s == expected), "missing {expected}: {m:?}");
        }
    }

    #[test]
    fn level_gating_matches_upstream() {
        // upstream: DEBUG_GTE(ACL, 1) - emission disabled at level 0.
        init_at(0);
        trace_default_perms_for_dir(0o755, "/tmp/dst");
        assert!(acl_messages().is_empty(), "level 0 must suppress emission");
    }

    #[test]
    fn level_one_emits() {
        // upstream: DEBUG_GTE(ACL, 1) - first level enables emission.
        init_at(1);
        trace_default_perms_for_dir(0o750, "/var/spool");
        let m = acl_messages();
        assert_eq!(m.len(), 1);
        assert_eq!(
            m[0],
            "got ACL-based default perms 750 for directory /var/spool"
        );
    }

    #[test]
    fn acl_uid_remap_wire_shape() {
        // upstream: uidlist.c:287-291 - "uid %u(%s) maps to %u"
        init_own_at(2);
        trace_acl_uid_remap(1000, "alice", 1500);
        trace_acl_uid_remap(99999, "ghost", 99999);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "uid 1000(alice) maps to 1500"),
            "missing alice mapping: {m:?}"
        );
        assert!(
            m.iter().any(|s| s == "uid 99999(ghost) maps to 99999"),
            "missing ghost fallthrough mapping: {m:?}"
        );
    }

    #[test]
    fn acl_gid_remap_wire_shape() {
        // upstream: uidlist.c:287-291 - "gid %u(%s) maps to %u"
        init_own_at(2);
        trace_acl_gid_remap(100, "users", 200);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "gid 100(users) maps to 200"),
            "missing gid mapping: {m:?}"
        );
    }

    #[test]
    fn acl_id_remap_gated_below_level_two() {
        // upstream: DEBUG_GTE(OWN, 2) - level 1 must suppress the emission.
        init_own_at(1);
        trace_acl_uid_remap(1000, "alice", 1500);
        trace_acl_gid_remap(100, "users", 200);
        assert!(
            own_messages().is_empty(),
            "level 1 must suppress OWN/2 emission"
        );
    }
}
