//! `--debug=HASH` producer emissions for the delta signature hash table.
//!
//! Mirrors upstream rsync's `hashtable.c` `DEBUG_GTE(HASH, 1)` output so
//! wire-comparable diagnostics align across implementations.
//!
//! # Upstream Reference
//!
//! - `hashtable.c:45-53`  - created hashtable emission.
//! - `hashtable.c:60-63`  - destroyed hashtable emission.
//! - `hashtable.c:100-103` - growing hashtable emission.
//!
//! Upstream prepends a `[<role>]` prefix derived from `who_am_i()`. The
//! [`HashtableRole`] enum exposes the same vocabulary so callers can pass
//! the role explicitly without depending on the protocol crate.

use logging::debug_log;

/// Process role used as the `[<role>]` prefix in HASH emissions.
///
/// Mirrors upstream's `who_am_i()` return strings (`sender`,
/// `receiver`, `generator`) without pulling in a heavier dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashtableRole {
    /// Sender role - builds the index after receiving signatures.
    Sender,
    /// Receiver role - builds the index during local delta apply.
    Receiver,
    /// Generator role - builds the index while emitting signatures.
    Generator,
}

impl HashtableRole {
    /// Returns the upstream-equivalent string for the role.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
            Self::Generator => "generator",
        }
    }
}

impl std::fmt::Display for HashtableRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Width, in bits, of the hash key stored in the delta signature index.
///
/// The index packs `(sum1, sum2)` into a single `u32`, matching upstream's
/// 32-bit key family (`HT_KEY32`) used for non-`hashlittle2`-style tables.
pub const HASH_KEY_BITS: u32 = 32;

/// Traces hashtable creation (level 1).
///
/// Matches upstream rsync exactly:
/// ```text
/// [sender] created hashtable 7f00aa00 (size: 4096, keys: 32-bit)
/// ```
///
/// When the requested capacity differs from the rounded-up table size, the
/// upstream variant inserts a `req: <n>, ` prefix inside the parentheses:
/// ```text
/// [sender] created hashtable 7f00aa00 (req: 50, size: 64, keys: 32-bit)
/// ```
///
/// upstream: hashtable.c:45-53.
#[inline]
pub fn trace_created(role: HashtableRole, id: usize, requested: usize, size: usize) {
    if requested != size {
        debug_log!(
            Hash,
            1,
            "[{}] created hashtable {:x} (req: {}, size: {}, keys: {}-bit)",
            role,
            id,
            requested,
            size,
            HASH_KEY_BITS
        );
    } else {
        debug_log!(
            Hash,
            1,
            "[{}] created hashtable {:x} (size: {}, keys: {}-bit)",
            role,
            id,
            size,
            HASH_KEY_BITS
        );
    }
}

/// Traces hashtable destruction (level 1).
///
/// Matches upstream rsync exactly:
/// ```text
/// [sender] destroyed hashtable 7f00aa00 (size: 4096, keys: 32-bit)
/// ```
///
/// upstream: hashtable.c:60-63.
#[inline]
pub fn trace_destroyed(role: HashtableRole, id: usize, size: usize) {
    debug_log!(
        Hash,
        1,
        "[{}] destroyed hashtable {:x} (size: {}, keys: {}-bit)",
        role,
        id,
        size,
        HASH_KEY_BITS
    );
}

/// Traces hashtable growth (level 1).
///
/// Matches upstream rsync exactly:
/// ```text
/// [sender] growing hashtable 7f00aa00 (size: 8192, keys: 32-bit)
/// ```
///
/// upstream: hashtable.c:100-103.
#[inline]
pub fn trace_growing(role: HashtableRole, id: usize, size: usize) {
    debug_log!(
        Hash,
        1,
        "[{}] growing hashtable {:x} (size: {}, keys: {}-bit)",
        role,
        id,
        size,
        HASH_KEY_BITS
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use logging::{DebugFlag, DiagnosticEvent, VerbosityConfig, drain_events, init};

    fn setup_hash(level: u8) {
        let mut cfg = VerbosityConfig::default();
        cfg.debug.hash = level;
        init(cfg);
        let _ = drain_events();
    }

    fn hash_messages() -> Vec<String> {
        drain_events()
            .into_iter()
            .filter_map(|event| match event {
                DiagnosticEvent::Debug {
                    flag: DebugFlag::Hash,
                    message,
                    ..
                } => Some(message),
                _ => None,
            })
            .collect()
    }

    /// Pins the level 1 created emission to upstream's format byte-for-byte
    /// when the requested capacity matches the rounded-up size.
    ///
    /// upstream: hashtable.c:45-53.
    #[test]
    fn created_matches_upstream_format_no_req() {
        setup_hash(1);
        trace_created(HashtableRole::Sender, 0x7f00_aa00, 4096, 4096);
        let msgs = hash_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "[sender] created hashtable 7f00aa00 (size: 4096, keys: 32-bit)"),
            "expected upstream-format HASH,1 created emission, got {msgs:?}"
        );
    }

    /// Pins the level 1 created emission with the `req:` prefix when the
    /// requested capacity differs from the rounded-up size.
    ///
    /// upstream: hashtable.c:46-50.
    #[test]
    fn created_matches_upstream_format_with_req() {
        setup_hash(1);
        trace_created(HashtableRole::Receiver, 0x4242, 50, 64);
        let msgs = hash_messages();
        assert!(
            msgs.iter().any(
                |m| m == "[receiver] created hashtable 4242 (req: 50, size: 64, keys: 32-bit)"
            ),
            "expected upstream-format HASH,1 created emission with req prefix, got {msgs:?}"
        );
    }

    /// Pins the level 1 destroyed emission to upstream's format.
    ///
    /// upstream: hashtable.c:60-63.
    #[test]
    fn destroyed_matches_upstream_format() {
        setup_hash(1);
        trace_destroyed(HashtableRole::Generator, 0xdead, 256);
        let msgs = hash_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "[generator] destroyed hashtable dead (size: 256, keys: 32-bit)"),
            "expected upstream-format HASH,1 destroyed emission, got {msgs:?}"
        );
    }

    /// Pins the level 1 growing emission to upstream's format.
    ///
    /// upstream: hashtable.c:100-103.
    #[test]
    fn growing_matches_upstream_format() {
        setup_hash(1);
        trace_growing(HashtableRole::Sender, 0xfeed, 8192);
        let msgs = hash_messages();
        assert!(
            msgs.iter()
                .any(|m| m == "[sender] growing hashtable feed (size: 8192, keys: 32-bit)"),
            "expected upstream-format HASH,1 growing emission, got {msgs:?}"
        );
    }

    /// HASH emissions must not fire when the flag is disabled.
    #[test]
    fn gated_by_debug_flag() {
        setup_hash(0);
        trace_created(HashtableRole::Sender, 1, 1, 1);
        trace_destroyed(HashtableRole::Sender, 1, 1);
        trace_growing(HashtableRole::Sender, 1, 1);
        assert!(hash_messages().is_empty());
    }

    /// Role display strings match upstream `who_am_i()` vocabulary.
    #[test]
    fn role_display_matches_who_am_i() {
        assert_eq!(HashtableRole::Sender.to_string(), "sender");
        assert_eq!(HashtableRole::Receiver.to_string(), "receiver");
        assert_eq!(HashtableRole::Generator.to_string(), "generator");
    }
}
