//! Stub for the Landlock LSM allowlist on non-Linux targets or when the
//! `landlock` Cargo feature is disabled.
//!
//! Mirrors the public surface of [`crate::landlock`] so cross-platform
//! callers compile without `#[cfg]` branching. Every entry point returns
//! `Unavailable` (or `false` for the probe) so the SEC-1 `*at` chain
//! remains the active defense. See
//! `docs/design/sec-1-p-landlock-defense-in-depth-2026-05-22.md`.

#![allow(dead_code)]

use std::io;
use std::path::Path;

/// Outcome of a [`restrict_to_module_paths`] call.
///
/// The stub only ever returns [`LandlockOutcome::Unavailable`]; the variants
/// match the Linux implementation so callers can pattern-match the same
/// types on every target.
#[derive(Debug)]
pub enum LandlockOutcome {
    /// Sandbox engaged. The stub never returns this variant; the field type
    /// mirrors the Linux side's `RulesetStatus` placeholder so signatures
    /// stay structurally identical.
    Enforced(EnforcementStatus),
    /// The kernel does not expose Landlock (or the Cargo feature is off).
    /// Always returned on this build.
    Unavailable,
    /// Ruleset setup failed even though the kernel advertised support.
    /// Never returned by the stub.
    Error(io::Error),
}

/// Placeholder for the `landlock::RulesetStatus` enum that the Linux build
/// carries. Kept as an opaque marker so call sites can `matches!` against
/// it without depending on the Linux-only crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnforcementStatus {
    /// All requested rights were honoured by the kernel.
    FullyEnforced,
    /// Best-effort downgrade applied a subset of the requested rights.
    PartiallyEnforced,
    /// The kernel accepted the ruleset but applied no restrictions.
    NotEnforced,
}

/// Always returns `false` on this build.
#[must_use]
pub fn is_supported() -> bool {
    false
}

/// Always returns [`LandlockOutcome::Unavailable`] on this build.
///
/// # Errors
///
/// The stub never returns the `Error` variant.
pub fn restrict_to_module_paths(_allowed_roots: &[&Path]) -> LandlockOutcome {
    LandlockOutcome::Unavailable
}

/// Always returns `None` on this build: Landlock is unavailable, so there is
/// no engaged ruleset that could have been downgraded. Mirrors the Linux
/// signature so the daemon reports downgrades without `#[cfg]` branching.
#[must_use]
pub fn best_effort_fs_downgrade() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn is_supported_returns_false() {
        assert!(!is_supported());
    }

    #[test]
    fn restrict_returns_unavailable() {
        let tmp = TempDir::new().expect("tempdir");
        let outcome = restrict_to_module_paths(&[tmp.path()]);
        assert!(matches!(outcome, LandlockOutcome::Unavailable));
    }
}
