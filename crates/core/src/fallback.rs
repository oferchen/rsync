#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `fallback` module centralises parsing of environment variables that
//! control whether the workspace should delegate remote transfers to the
//! upstream `rsync` binary. Both the client and daemon binaries honour the
//! `OC_RSYNC_FALLBACK` and `OC_RSYNC_DAEMON_FALLBACK` overrides, allowing users
//! to explicitly disable delegation, select the default `rsync` executable, or
//! provide a custom path. Keeping the parsing logic in a shared module avoids
//! subtle discrepancies across crates and ensures unit tests cover every
//! interpretation rule.
//!
//! # Design
//!
//! The module exposes [`fallback_override`] to decode a single environment
//! variable into a [`FallbackOverride`]. Callers can chain multiple invocations
//! to honour primary and secondary environment names before deciding whether a
//! delegation binary is available. The [`FallbackOverride::resolve_or_default`]
//! helper converts the parsed override into an [`OsString`] when delegation is
//! enabled, falling back to the supplied default executable (`rsync`) when the
//! override is `auto` or unspecified.
//!
//! # Invariants
//!
//! - Whitespace-only values are treated as disabled overrides, matching the
//!   daemon's historical behaviour.
//! - The case-insensitive strings `0`, `false`, `no`, and `off` disable
//!   delegation.
//! - The special value `auto` resolves to the default executable supplied by
//!   the caller (`rsync`).
//!
//! # Errors
//!
//! The helpers do not construct [`Message`](crate::message::Message) instances
//! because callers must decide how to report disabled delegation. Instead, the
//! parsing functions return [`None`] when an override is not present or
//! [`FallbackOverride::Disabled`] when delegation is explicitly turned off.
//!
//! # Examples
//!
//! Determine which override applies and resolve it to an executable path:
//!
//! ```
//! use rsync_core::fallback::{fallback_override, FallbackOverride};
//! use std::ffi::OsStr;
//!
//! std::env::set_var("OC_RSYNC_FALLBACK", "auto");
//! let override_value = fallback_override("OC_RSYNC_FALLBACK").unwrap();
//! assert_eq!(
//!     override_value.resolve_or_default(OsStr::new("rsync")),
//!     Some("rsync".into())
//! );
//! std::env::remove_var("OC_RSYNC_FALLBACK");
//! ```
//!
//! # See also
//!
//! - [`rsync_core::client::run_remote_transfer_fallback`] for the primary
//!   consumer of these helpers on the client side.
//! - [`rsync_daemon::run`] for daemon-side delegation.

use std::env;
use std::ffi::{OsStr, OsString};

/// Represents the parsed interpretation of a fallback environment override.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FallbackOverride {
    /// Delegation is explicitly disabled.
    Disabled,
    /// Delegation should use the caller-provided default executable.
    Default,
    /// Delegation should execute the supplied program path.
    Explicit(OsString),
}

impl FallbackOverride {
    /// Resolves the override into an executable path.
    ///
    /// When the override is [`FallbackOverride::Disabled`] the function returns
    /// [`None`]. Otherwise the resolved executable is returned, with
    /// [`FallbackOverride::Default`] mapping to the provided default.
    #[must_use]
    pub fn resolve_or_default(self, default: &OsStr) -> Option<OsString> {
        match self {
            Self::Disabled => None,
            Self::Default => Some(default.to_os_string()),
            Self::Explicit(path) => Some(path),
        }
    }
}

/// Parses the supplied environment variable into a [`FallbackOverride`].
///
/// The helper returns [`None`] when the variable is unset. When set, the
/// following interpretations apply:
///
/// - Empty or whitespace-only values disable delegation.
/// - `0`, `false`, `no`, and `off` (case-insensitive) disable delegation.
/// - `auto` selects the caller-provided default executable.
/// - Any other value is treated as an explicit binary path.
#[must_use]
pub fn fallback_override(name: &str) -> Option<FallbackOverride> {
    let raw = env::var_os(name)?;

    if raw.is_empty() {
        return Some(FallbackOverride::Disabled);
    }

    if let Some(text) = raw.to_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Some(FallbackOverride::Disabled);
        }

        let lowered = trimmed.to_ascii_lowercase();
        if matches!(lowered.as_str(), "0" | "false" | "no" | "off") {
            return Some(FallbackOverride::Disabled);
        }

        if lowered == "auto" {
            return Some(FallbackOverride::Default);
        }
    }

    Some(FallbackOverride::Explicit(raw))
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;

    fn clear_var(key: &str) {
        unsafe {
            env::remove_var(key);
        }
    }

    fn set_var(key: &str, value: &str) {
        unsafe {
            env::set_var(key, value);
        }
    }

    #[test]
    fn unset_variable_returns_none() {
        let key = "RSYNC_FALLBACK_TEST_UNSET";
        clear_var(key);
        assert!(fallback_override(key).is_none());
    }

    #[test]
    fn empty_value_disables_override() {
        let key = "RSYNC_FALLBACK_TEST_EMPTY";
        set_var(key, "");
        assert_eq!(fallback_override(key), Some(FallbackOverride::Disabled));
        clear_var(key);
    }

    #[test]
    fn boolean_false_values_disable_override() {
        for value in ["0", "false", "No", "OFF"] {
            let key = "RSYNC_FALLBACK_TEST_BOOL";
            set_var(key, value);
            assert_eq!(fallback_override(key), Some(FallbackOverride::Disabled));
            clear_var(key);
        }
    }

    #[test]
    fn auto_value_uses_default() {
        let key = "RSYNC_FALLBACK_TEST_AUTO";
        set_var(key, "auto");
        let override_value = fallback_override(key).expect("override present");
        assert_eq!(
            override_value.resolve_or_default(OsStr::new("rsync")),
            Some(OsString::from("rsync"))
        );
        clear_var(key);
    }

    #[test]
    fn explicit_value_preserved() {
        let key = "RSYNC_FALLBACK_TEST_EXPLICIT";
        set_var(key, "/usr/bin/rsync");
        let override_value = fallback_override(key).expect("override present");
        assert_eq!(
            override_value.resolve_or_default(OsStr::new("rsync")),
            Some(OsString::from("/usr/bin/rsync"))
        );
        clear_var(key);
    }
}
