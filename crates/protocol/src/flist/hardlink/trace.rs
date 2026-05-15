//! `--debug=HLINK` producer emissions for hardlink processing.
//!
//! Matches upstream rsync's `hlink.c` DEBUG_GTE(HLINK, N) output byte-for-byte
//! so wire-comparable diagnostics align across implementations.
//!
//! # Upstream Reference
//!
//! - upstream: hlink.c HLINK debug emissions (`idev_find`, `hard_link_check`).

use logging::debug_log;

use crate::flist::trace::ProcessRole;

/// Big-number formatting matching upstream's `big_num()` helper used in HLINK
/// messages for device identifiers.
fn big_num(value: u64) -> String {
    let s = value.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len() + bytes.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (len - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

/// Traces hash-bucket creation for a new device (level 3).
///
/// upstream: hlink.c:77 - `"[%s] created hashtable for dev %s"`.
#[inline]
pub fn trace_hashtable_for_dev(role: ProcessRole, dev: u64) {
    debug_log!(
        Hlink,
        3,
        "[{}] created hashtable for dev {}",
        role,
        big_num(dev)
    );
}

/// Traces a follower whose prior leader was skipped (level 2).
///
/// upstream: hlink.c:305 - `"hlink for %d (%s,%d): virtual first"`.
#[inline]
pub fn trace_virtual_first(ndx: i32, name: &str, gnum: i32) {
    debug_log!(
        Hlink,
        2,
        "hlink for {} ({},{}): virtual first",
        ndx,
        name,
        gnum
    );
}

/// Traces a follower deferred while the leader transfer is in flight (level 2).
///
/// upstream: hlink.c:325 - `"hlink for %d (%s,%d): waiting for %d"`.
#[inline]
pub fn trace_waiting_for(ndx: i32, name: &str, gnum: i32, prev_ndx: i32) {
    debug_log!(
        Hlink,
        2,
        "hlink for {} ({},{}): waiting for {}",
        ndx,
        name,
        gnum,
        prev_ndx
    );
}

/// Traces a follower searching for an unassigned leader (level 2).
///
/// upstream: hlink.c:331 - `"hlink for %d (%s,%d): looking for a leader"`.
#[inline]
pub fn trace_looking_for_leader(ndx: i32, name: &str, gnum: i32) {
    debug_log!(
        Hlink,
        2,
        "hlink for {} ({},{}): looking for a leader",
        ndx,
        name,
        gnum
    );
}

/// Traces a follower matched against an alt-dest file (level 2).
///
/// upstream: hlink.c:353 - `"hlink for %d (%s,%d): found flist match (alt %d)"`.
#[inline]
pub fn trace_found_flist_match(ndx: i32, name: &str, gnum: i32, alt_dest: i32) {
    debug_log!(
        Hlink,
        2,
        "hlink for {} ({},{}): found flist match (alt {})",
        ndx,
        name,
        gnum,
        alt_dest
    );
}

/// Traces final leader resolution for a follower (level 2).
///
/// upstream: hlink.c:370 - `"hlink for %d (%s,%d): leader is %d (%s)"`.
#[inline]
pub fn trace_leader_is(ndx: i32, name: &str, gnum: i32, prev_ndx: i32, prev_name: &str) {
    debug_log!(
        Hlink,
        2,
        "hlink for {} ({},{}): leader is {} ({})",
        ndx,
        name,
        gnum,
        prev_ndx,
        prev_name
    );
}

#[cfg(test)]
mod tests {
    //! Pinning tests for HLINK emission shapes. Strings match upstream
    //! `hlink.c` byte-for-byte.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.hlink = level;
        init(cfg);
        let _ = drain_events();
    }

    fn hlink_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Hlink,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn upstream_wire_shapes() {
        // upstream: hlink.c:78,306,326,332,354,371 - each HLINK debug emission.
        assert_eq!(big_num(2_049_000_000), "2,049,000,000");

        init_at(3);
        trace_hashtable_for_dev(ProcessRole::Sender, 2049);
        trace_virtual_first(7, "dir/file", 3);
        trace_waiting_for(9, "dir/file", 3, 5);
        trace_looking_for_leader(11, "dir/file", 4);
        trace_found_flist_match(12, "dir/file", 4, 1);
        trace_leader_is(13, "dir/file", 4, 8, "dir/leader");

        let m = hlink_messages();
        for expected in [
            "[sender] created hashtable for dev 2,049",
            "hlink for 7 (dir/file,3): virtual first",
            "hlink for 9 (dir/file,3): waiting for 5",
            "hlink for 11 (dir/file,4): looking for a leader",
            "hlink for 12 (dir/file,4): found flist match (alt 1)",
            "hlink for 13 (dir/file,4): leader is 8 (dir/leader)",
        ] {
            assert!(m.iter().any(|s| s == expected), "missing {expected}: {m:?}");
        }
    }

    #[test]
    fn level_gating_matches_upstream() {
        // upstream: DEBUG_GTE(HLINK, N) gates each emission.
        init_at(1);
        trace_virtual_first(1, "f", 0);
        trace_hashtable_for_dev(ProcessRole::Sender, 1);
        assert!(hlink_messages().is_empty(), "level 2/3 must be gated");
    }
}
