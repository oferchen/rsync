//! `--debug=OWN` producer emissions for uid/gid name <-> id mapping.
//!
//! Matches upstream rsync's `uidlist.c` and `rsync.c` `DEBUG_GTE(OWN, N)`
//! output byte-for-byte so wire-comparable diagnostics align across
//! implementations.
//!
//! # Upstream Reference
//!
//! - `uidlist.c:218-227` (`DEBUG_GTE(OWN, 2)`) - `is_in_group` probe of the
//!   calling process's gid list. Emitted once per process when `--group`
//!   ownership preservation runs without root and the gidset is built.
//!   Shape: `"process has %d gid%s: %d %d ..."`.
//! - `uidlist.c:287-291` (`DEBUG_GTE(OWN, 2)`) - `recv_add_id` per-entry
//!   mapping trace. Emitted whenever the receiver adds a uid or gid to
//!   the id list. Shape: `"%sid %u(%s) maps to %u"` where the leading
//!   character is `'u'` for the uid list and `'g'` for the gid list.
//! - `rsync.c:535-545` (`DEBUG_GTE(OWN, 1)`) - chown reporting from
//!   `set_file_attrs`. Emitted once per file when the resolved uid or
//!   gid differs from the destination's existing uid/gid. Shapes:
//!   `"set uid of %s from %u to %u"` and `"set gid of %s from %u to %u"`.
//! - `options.c:307` - `DEBUG_WORD(OWN, W_REC, "Debug ownership changes
//!   in users & groups (levels 1-2)")` flag table entry, capping
//!   emissions at level 2.

use logging::debug_log;

/// Identifies whether an emission refers to the uid list or the gid list.
///
/// Mirrors upstream's `idlist_ptr == &uidlist ? "u" : "g"` switch in
/// `uidlist.c:289`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    /// User-id list (upstream: `&uidlist`).
    Uid,
    /// Group-id list (upstream: `&gidlist`).
    Gid,
}

impl IdKind {
    /// Returns the upstream single-character prefix (`"u"` or `"g"`).
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Uid => "u",
            Self::Gid => "g",
        }
    }

    /// Returns the upstream lowercase word (`"uid"` or `"gid"`) used in
    /// the level-1 chown trace.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Uid => "uid",
            Self::Gid => "gid",
        }
    }
}

/// Traces a single uid/gid mapping decision (level 2).
///
/// upstream: `uidlist.c:287-291` - `"%sid %u(%s) maps to %u\n"`. The `kind`
/// argument selects the upstream `"u"`/`"g"` prefix, `remote_id` is the id
/// that arrived from the peer, `name` is the resolved name (rendered as
/// the empty string when `None`, matching upstream's `name ? name : ""`
/// at `uidlist.c:252-253`), and `local_id` is the id the receiver will
/// use locally.
#[inline]
pub fn trace_id_maps_to(kind: IdKind, remote_id: u32, name: Option<&[u8]>, local_id: u32) {
    let name_str = name.map(|bytes| String::from_utf8_lossy(bytes));
    let name_rendered: &str = name_str.as_deref().unwrap_or("");
    debug_log!(
        Own,
        2,
        "{}id {}({}) maps to {}",
        kind.prefix(),
        remote_id,
        name_rendered,
        local_id
    );
}

/// Traces the calling process's supplementary group list (level 2).
///
/// upstream: `uidlist.c:218-227` - `"process has %d gid%s: %d %d ..."`.
/// Emitted once per process from `is_in_group` after `getgroups` populates
/// the gid set. The plural `"s"` is appended when `gids.len() != 1`,
/// matching upstream's `ngroups == 1 ? "" : "s"` ternary.
#[inline]
pub fn trace_process_gids(gids: &[u32]) {
    let plural = if gids.len() == 1 { "" } else { "s" };
    let mut rendered = String::with_capacity(gids.len() * 6);
    for gid in gids {
        rendered.push(' ');
        rendered.push_str(&gid.to_string());
    }
    debug_log!(
        Own,
        2,
        "process has {} gid{}:{}",
        gids.len(),
        plural,
        rendered
    );
}

/// Traces a uid ownership change about to be applied (level 1).
///
/// upstream: `rsync.c:537-540` - `"set uid of %s from %u to %u\n"`.
/// Emitted by `set_file_attrs` immediately before `do_lchown` when the
/// resolved uid differs from the destination's existing uid.
#[inline]
pub fn trace_set_uid(fname: &str, from: u32, to: u32) {
    debug_log!(Own, 1, "set uid of {} from {} to {}", fname, from, to);
}

/// Traces a gid ownership change about to be applied (level 1).
///
/// upstream: `rsync.c:541-545` - `"set gid of %s from %u to %u\n"`.
/// Emitted by `set_file_attrs` immediately before `do_lchown` when the
/// resolved gid differs from the destination's existing gid.
#[inline]
pub fn trace_set_gid(fname: &str, from: u32, to: u32) {
    debug_log!(Own, 1, "set gid of {} from {} to {}", fname, from, to);
}

#[cfg(test)]
mod tests {
    //! Pinning tests for OWN emission shapes. Strings match upstream
    //! `uidlist.c` and `rsync.c` byte-for-byte.

    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn init_at(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.own = level;
        init(cfg);
        let _ = drain_events();
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

    #[test]
    fn kind_prefix_and_word_match_upstream() {
        // upstream: uidlist.c:289 - `idlist_ptr == &uidlist ? "u" : "g"`.
        assert_eq!(IdKind::Uid.prefix(), "u");
        assert_eq!(IdKind::Gid.prefix(), "g");
        // upstream: rsync.c:538,542 - `"set uid of"` / `"set gid of"`.
        assert_eq!(IdKind::Uid.word(), "uid");
        assert_eq!(IdKind::Gid.word(), "gid");
    }

    #[test]
    fn level2_uid_maps_to_named() {
        // upstream: uidlist.c:287-291 - "uid 1000(alice) maps to 500".
        init_at(2);
        trace_id_maps_to(IdKind::Uid, 1000, Some(b"alice"), 500);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "uid 1000(alice) maps to 500"),
            "missing uid map: {m:?}"
        );
    }

    #[test]
    fn level2_gid_maps_to_named() {
        // upstream: uidlist.c:287-291 with `idlist_ptr == &gidlist`.
        init_at(2);
        trace_id_maps_to(IdKind::Gid, 100, Some(b"users"), 20);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "gid 100(users) maps to 20"),
            "missing gid map: {m:?}"
        );
    }

    #[test]
    fn level2_maps_to_empty_name() {
        // upstream: uidlist.c:252-253 - `if (!name) name = ""`.
        init_at(2);
        trace_id_maps_to(IdKind::Uid, 0, None, 0);
        trace_id_maps_to(IdKind::Gid, 42, Some(b""), 42);
        let m = own_messages();
        assert!(m.iter().any(|s| s == "uid 0() maps to 0"), "{m:?}");
        assert!(m.iter().any(|s| s == "gid 42() maps to 42"), "{m:?}");
    }

    #[test]
    fn level2_process_gids_singular() {
        // upstream: uidlist.c:221 - `ngroups == 1 ? "" : "s"`.
        init_at(2);
        trace_process_gids(&[1000]);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "process has 1 gid: 1000"),
            "missing singular gid line: {m:?}"
        );
    }

    #[test]
    fn level2_process_gids_plural() {
        // upstream: uidlist.c:221-225 - "process has N gids: g1 g2 ...".
        init_at(2);
        trace_process_gids(&[0, 4, 100, 1000]);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "process has 4 gids: 0 4 100 1000"),
            "missing plural gid line: {m:?}"
        );
    }

    #[test]
    fn level1_set_uid_emits() {
        // upstream: rsync.c:537-540 - "set uid of %s from %u to %u".
        init_at(1);
        trace_set_uid("/tmp/dst", 1000, 0);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "set uid of /tmp/dst from 1000 to 0"),
            "missing set-uid line: {m:?}"
        );
    }

    #[test]
    fn level1_set_gid_emits() {
        // upstream: rsync.c:541-545 - "set gid of %s from %u to %u".
        init_at(1);
        trace_set_gid("./file", 100, 200);
        let m = own_messages();
        assert!(
            m.iter().any(|s| s == "set gid of ./file from 100 to 200"),
            "missing set-gid line: {m:?}"
        );
    }

    #[test]
    fn level1_gates_level2_emissions() {
        // upstream: DEBUG_GTE(OWN, 2) gates the per-entry mapping.
        init_at(1);
        trace_id_maps_to(IdKind::Uid, 1, Some(b"r"), 1);
        trace_process_gids(&[0]);
        assert!(
            own_messages().is_empty(),
            "level-2 emissions must be gated at level 1"
        );
    }

    #[test]
    fn level0_suppresses_all_own_emissions() {
        // upstream: with DEBUG_OWN at level 0, every DEBUG_GTE(OWN, N)
        // evaluates to false.
        init_at(0);
        trace_id_maps_to(IdKind::Uid, 1, Some(b"r"), 1);
        trace_process_gids(&[0]);
        trace_set_uid("a", 0, 1);
        trace_set_gid("a", 0, 1);
        assert!(
            own_messages().is_empty(),
            "all OWN emissions must be gated at level 0"
        );
    }
}
