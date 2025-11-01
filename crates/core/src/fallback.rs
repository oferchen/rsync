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
//! The module exposes [`fallback_override`](crate::fallback::fallback_override)
//! to decode a single environment variable into a
//! [`FallbackOverride`](crate::fallback::FallbackOverride). Callers can chain
//! multiple invocations to honour primary and secondary environment names
//! before deciding whether a delegation binary is available. The
//! [`crate::fallback::FallbackOverride::resolve_or_default`] helper converts the
//! parsed override into an [`std::ffi::OsString`] when delegation is enabled,
//! falling back to the
//! supplied default executable (`rsync`) when the override is `auto` or
//! unspecified.
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
//! The helpers do not construct [`crate::message::Message`] instances
//! because callers must decide how to report disabled delegation. Instead, the
//! parsing functions return [`None`] when an override is not present or
//! [`crate::fallback::FallbackOverride::Disabled`] when delegation is explicitly
//! turned off.
//!
//! # Examples
//!
//! Construct overrides directly and resolve them to executable paths. In real
//! usage call [`fallback_override`](crate::fallback::fallback_override) to parse
//! environment variables into the [`FallbackOverride`](crate::fallback::FallbackOverride)
//! enum before invoking
//! [`crate::fallback::FallbackOverride::resolve_or_default`].
//!
//! ```
//! use rsync_core::fallback::FallbackOverride;
//! use std::ffi::OsStr;
//!
//! let default = FallbackOverride::Default;
//! assert_eq!(
//!     default.resolve_or_default(OsStr::new("rsync")),
//!     Some("rsync".into())
//! );
//!
//! let explicit = FallbackOverride::Explicit("/usr/bin/rsync".into());
//! assert_eq!(
//!     explicit.resolve_or_default(OsStr::new("rsync")),
//!     Some("/usr/bin/rsync".into())
//! );
//! ```
//!
//! # See also
//!
//! - [`crate::client::run_remote_transfer_fallback`] for the primary consumer of
//!   these helpers on the client side.
//! - `rsync_daemon::run` for daemon-side delegation.

mod binary;

use std::env;
use std::ffi::{OsStr, OsString};

pub use binary::{
    describe_missing_fallback_binary, fallback_binary_available, fallback_binary_candidates,
};

/// Name of the client fallback override environment variable.
///
/// The `OC_RSYNC_FALLBACK` variable matches the historical environment knob
/// used by packaging to control remote-shell delegation. Exposing the name as
/// a constant keeps binaries and tests in sync when future rebranding requires
/// updating the identifier in one place.
pub const CLIENT_FALLBACK_ENV: &str = "OC_RSYNC_FALLBACK";

/// Name of the daemon-specific fallback override environment variable.
///
/// When present, the value controls whether `oc-rsyncd` should delegate module
/// sessions to the upstream `rsync` binary. The helper mirrors the
/// workspace-wide client override ([`CLIENT_FALLBACK_ENV`]) while allowing
/// operators to toggle delegation independently for the daemon process.
pub const DAEMON_FALLBACK_ENV: &str = "OC_RSYNC_DAEMON_FALLBACK";

/// Name of the daemon auto-delegation environment variable.
///
/// Setting [`DAEMON_AUTO_DELEGATE_ENV`] to a truthy value enables the
/// compatibility mode where `oc-rsyncd` launches the upstream `rsync` binary
/// without requiring the `--delegate-system-rsync` flag. The constant lives in
/// this module so callers and integration tests share the same identifier when
/// toggling the behaviour.
pub const DAEMON_AUTO_DELEGATE_ENV: &str = "OC_RSYNC_DAEMON_AUTO_DELEGATE";

const DISABLED_OVERRIDES: [&str; 4] = ["0", "false", "no", "off"];
const DEFAULT_OVERRIDES: [&str; 6] = ["1", "true", "yes", "on", "auto", "default"];

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
/// - `1`, `true`, `yes`, and `on` (case-insensitive) select the caller-provided default executable.
/// - `auto` selects the caller-provided default executable.
/// - `default` selects the caller-provided default executable.
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

        if matches_ascii_case(trimmed, &DISABLED_OVERRIDES) {
            return Some(FallbackOverride::Disabled);
        }

        if matches_ascii_case(trimmed, &DEFAULT_OVERRIDES) {
            return Some(FallbackOverride::Default);
        }
    }

    Some(FallbackOverride::Explicit(raw))
}

fn matches_ascii_case(value: &str, options: &[&str]) -> bool {
    options
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    struct EnvVarGuard {
        key: OsString,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        #[allow(unsafe_code)]
        fn remove<K>(key: K) -> Self
        where
            K: Into<OsString>,
        {
            let key = key.into();
            let original = env::var_os(&key);
            unsafe {
                env::remove_var(&key);
            }
            Self { key, original }
        }

        #[allow(unsafe_code)]
        fn set<K, V>(key: K, value: V) -> Self
        where
            K: Into<OsString>,
            V: Into<OsString>,
        {
            let key = key.into();
            let original = env::var_os(&key);
            unsafe {
                env::set_var(&key, value.into());
            }
            Self { key, original }
        }
    }

    #[test]
    fn fallback_env_constants_match_expected() {
        assert_eq!(CLIENT_FALLBACK_ENV, "OC_RSYNC_FALLBACK");
        assert_eq!(DAEMON_FALLBACK_ENV, "OC_RSYNC_DAEMON_FALLBACK");
        assert_eq!(DAEMON_AUTO_DELEGATE_ENV, "OC_RSYNC_DAEMON_AUTO_DELEGATE");
    }

    impl Drop for EnvVarGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            if let Some(value) = self.original.as_ref() {
                unsafe {
                    env::set_var(&self.key, value);
                }
            } else {
                unsafe {
                    env::remove_var(&self.key);
                }
            }
        }
    }

    #[test]
    fn unset_variable_returns_none() {
        let key = "RSYNC_FALLBACK_TEST_UNSET";
        let _guard = EnvVarGuard::remove(key);
        assert!(fallback_override(key).is_none());
    }

    #[test]
    fn empty_value_disables_override() {
        let key = "RSYNC_FALLBACK_TEST_EMPTY";
        let _guard = EnvVarGuard::set(key, "");
        assert_eq!(fallback_override(key), Some(FallbackOverride::Disabled));
    }

    #[test]
    fn boolean_false_values_disable_override() {
        let key = "RSYNC_FALLBACK_TEST_BOOL";
        let _reset = EnvVarGuard::remove(key);
        for value in ["0", "false", "No", "OFF"] {
            let _guard = EnvVarGuard::set(key, value);
            assert_eq!(fallback_override(key), Some(FallbackOverride::Disabled));
        }
    }

    #[test]
    fn auto_value_uses_default() {
        let key = "RSYNC_FALLBACK_TEST_AUTO";
        let _guard = EnvVarGuard::set(key, "auto");
        let override_value = fallback_override(key).expect("override present");
        assert_eq!(
            override_value.resolve_or_default(OsStr::new("rsync")),
            Some(OsString::from("rsync"))
        );
    }

    #[test]
    fn explicit_value_preserved() {
        let key = "RSYNC_FALLBACK_TEST_EXPLICIT";
        let _guard = EnvVarGuard::set(key, "/usr/bin/rsync");
        let override_value = fallback_override(key).expect("override present");
        assert_eq!(
            override_value.resolve_or_default(OsStr::new("rsync")),
            Some(OsString::from("/usr/bin/rsync"))
        );
    }

    #[test]
    fn auto_value_is_case_insensitive_and_trims_whitespace() {
        let key = "RSYNC_FALLBACK_TEST_AUTO_CASE";
        let _reset = EnvVarGuard::remove(key);
        for value in ["AUTO", " Auto", "auto  ", "  aUtO  "] {
            let _guard = EnvVarGuard::set(key, value);
            let override_value = fallback_override(key).expect("override present");
            assert_eq!(
                override_value.resolve_or_default(OsStr::new("rsync")),
                Some(OsString::from("rsync"))
            );
        }
    }

    #[test]
    fn default_value_behaves_like_auto() {
        let key = "RSYNC_FALLBACK_TEST_DEFAULT";
        let _reset = EnvVarGuard::remove(key);
        for value in ["default", "DEFAULT", " Default "] {
            let _guard = EnvVarGuard::set(key, value);
            let override_value = fallback_override(key).expect("override present");
            assert_eq!(
                override_value.resolve_or_default(OsStr::new("rsync")),
                Some(OsString::from("rsync"))
            );
        }
    }

    #[test]
    fn boolean_true_values_enable_default_delegate() {
        let key = "RSYNC_FALLBACK_TEST_TRUE";
        let _reset = EnvVarGuard::remove(key);
        for value in ["1", "true", "Yes", "ON"] {
            let _guard = EnvVarGuard::set(key, value);
            let override_value = fallback_override(key).expect("override present");
            assert_eq!(
                override_value.resolve_or_default(OsStr::new("rsync")),
                Some(OsString::from("rsync"))
            );
        }
    }

    #[test]
    fn disabled_values_trim_whitespace() {
        let key = "RSYNC_FALLBACK_TEST_DISABLED_TRIM";
        let _reset = EnvVarGuard::remove(key);
        for value in ["  no  ", " Off\t", "\n0\r", " false "] {
            let _guard = EnvVarGuard::set(key, value);
            assert_eq!(fallback_override(key), Some(FallbackOverride::Disabled));
        }
    }
}
