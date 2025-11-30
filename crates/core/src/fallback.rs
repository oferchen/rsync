#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! The `fallback` module centralises parsing of environment variables that
//! control whether the workspace should delegate remote transfers to the
//! upstream `rsync` binary. Both the client and daemon binaries honour the
//! `OC_RSYNC_FALLBACK` and `OC_RSYNC_DAEMON_FALLBACK` overrides, allowing users
//! to explicitly disable delegation, select the default `rsync` executable, or
//! provide a custom path. Setting `OC_RSYNC_DISABLE_FALLBACK` to a truthy value
//! hard-disables delegation regardless of per-role overrides. Keeping the
//! parsing logic in a shared module avoids subtle discrepancies across crates
//! and ensures unit tests cover every interpretation rule.
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
//! - Matching single or double quotes are stripped from values before
//!   interpretation so environment overrides such as `"/usr/bin/rsync"` are
//!   accepted without manual trimming.
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
//! use core::fallback::FallbackOverride;
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
//! - `daemon::run` for daemon-side delegation.

mod binary;

use std::env;
use std::ffi::{OsStr, OsString};

pub use binary::{
    describe_missing_fallback_binary, fallback_binary_available, fallback_binary_candidates,
    fallback_binary_is_self, fallback_binary_path,
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

/// Name of the workspace-wide environment flag that disables fallback execution.
///
/// When set to any truthy value the client and daemon refuse to spawn the system
/// `rsync` binary, surfacing a clear diagnostic instead. Explicitly false values
/// (`0`, `false`, `no`, or `off`) re-enable delegation, allowing operators to
/// toggle the behaviour without editing CLI arguments.
pub const DISABLE_FALLBACK_ENV: &str = "OC_RSYNC_DISABLE_FALLBACK";

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

/// Interprets an override value using the same precedence rules as
/// [`fallback_override`].
///
/// The function accepts borrowed values so configuration sources besides the
/// process environment (for example, command-line flags or configuration files)
/// can reuse the parsing logic without reimplementing the precedence rules.
///
/// # Examples
///
/// ```
/// use core::fallback::{interpret_override_value, FallbackOverride};
/// use std::ffi::OsStr;
///
/// assert_eq!(
///     interpret_override_value(OsStr::new("auto")),
///     FallbackOverride::Default
/// );
/// assert_eq!(
///     interpret_override_value(OsStr::new("/custom/rsync")),
///     FallbackOverride::Explicit("/custom/rsync".into())
/// );
/// ```
#[must_use]
pub fn interpret_override_value(raw: &OsStr) -> FallbackOverride {
    if raw.is_empty() {
        return FallbackOverride::Disabled;
    }

    if let Some(text) = raw.to_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return FallbackOverride::Disabled;
        }

        let (candidate, quoted) = match strip_enclosing_quotes(trimmed) {
            Some(inner) => (inner, true),
            None => (trimmed, false),
        };

        let normalized_candidate = candidate.trim();

        if normalized_candidate.is_empty() {
            return FallbackOverride::Disabled;
        }

        if matches_ascii_case(normalized_candidate, &DISABLED_OVERRIDES) {
            return FallbackOverride::Disabled;
        }

        if matches_ascii_case(normalized_candidate, &DEFAULT_OVERRIDES) {
            return FallbackOverride::Default;
        }

        if quoted {
            return FallbackOverride::Explicit(OsString::from(candidate));
        }
    }

    FallbackOverride::Explicit(raw.to_os_string())
}

/// Reports whether fallback execution is disabled via
/// [`DISABLE_FALLBACK_ENV`].
#[must_use]
pub fn fallback_invocation_disabled() -> bool {
    env::var_os(DISABLE_FALLBACK_ENV)
        .as_deref()
        .map(env_flag_truthy)
        .unwrap_or(false)
}

/// Provides a human-readable explanation when fallback execution is disabled.
#[must_use]
pub fn fallback_disabled_reason() -> Option<String> {
    fallback_invocation_disabled().then(|| {
        format!("fallback to the system rsync binary is disabled via {DISABLE_FALLBACK_ENV}")
    })
}

/// Parses the supplied environment variable into a [`FallbackOverride`].
///
/// The helper returns [`None`] when the variable is unset and otherwise defers
/// to [`interpret_override_value`] for the precedence rules documented there.
#[must_use]
pub fn fallback_override(name: &str) -> Option<FallbackOverride> {
    env::var_os(name).map(|raw| interpret_override_value(raw.as_os_str()))
}

fn matches_ascii_case(value: &str, options: &[&str]) -> bool {
    options
        .iter()
        .any(|candidate| value.eq_ignore_ascii_case(candidate))
}

fn env_flag_truthy(raw: &OsStr) -> bool {
    if raw.is_empty() {
        return true;
    }

    if let Some(text) = raw.to_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return true;
        }

        return !matches_ascii_case(trimmed, &DISABLED_OVERRIDES);
    }

    true
}

fn strip_enclosing_quotes(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.len() < 2 {
        return None;
    }

    let first = bytes[0];
    let last = *bytes.last().expect("value has at least two bytes");
    if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
        return Some(&value[1..value.len() - 1]);
    }

    None
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
    fn interpret_override_value_identifies_disabled_inputs() {
        assert_eq!(
            interpret_override_value(OsStr::new("")),
            FallbackOverride::Disabled
        );
        assert_eq!(
            interpret_override_value(OsStr::new("   ")),
            FallbackOverride::Disabled
        );
        assert_eq!(
            interpret_override_value(OsStr::new("No")),
            FallbackOverride::Disabled
        );
    }

    #[test]
    fn interpret_override_value_identifies_default_inputs() {
        assert_eq!(
            interpret_override_value(OsStr::new("1")),
            FallbackOverride::Default
        );
        assert_eq!(
            interpret_override_value(OsStr::new("auto")),
            FallbackOverride::Default
        );
        assert_eq!(
            interpret_override_value(OsStr::new(" Default ")),
            FallbackOverride::Default
        );
    }

    #[test]
    fn interpret_override_value_preserves_explicit_paths() {
        let explicit = interpret_override_value(OsStr::new("/usr/bin/rsync"));
        assert_eq!(
            explicit,
            FallbackOverride::Explicit(OsString::from("/usr/bin/rsync"))
        );

        let spaced = interpret_override_value(OsStr::new("  /opt/rsync  "));
        assert_eq!(
            spaced,
            FallbackOverride::Explicit(OsString::from("  /opt/rsync  "))
        );
    }

    #[test]
    fn interpret_override_value_handles_quoted_defaults() {
        assert_eq!(
            interpret_override_value(OsStr::new("\"auto\"")),
            FallbackOverride::Default
        );
        assert_eq!(
            interpret_override_value(OsStr::new("'FALSE'")),
            FallbackOverride::Disabled
        );
    }

    #[test]
    fn interpret_override_value_trims_keywords_within_quotes() {
        assert_eq!(
            interpret_override_value(OsStr::new("\" auto \"")),
            FallbackOverride::Default
        );
        assert_eq!(
            interpret_override_value(OsStr::new("' off  '")),
            FallbackOverride::Disabled
        );
    }

    #[test]
    fn interpret_override_value_strips_matching_quotes() {
        assert_eq!(
            interpret_override_value(OsStr::new("\"/usr/bin/rsync\"")),
            FallbackOverride::Explicit(OsString::from("/usr/bin/rsync"))
        );
        assert_eq!(
            interpret_override_value(OsStr::new("  'C:\\Program Files\\Rsync\\rsync.exe'  ")),
            FallbackOverride::Explicit(OsString::from("C:\\Program Files\\Rsync\\rsync.exe"))
        );
    }

    #[test]
    fn interpret_override_value_preserves_inner_whitespace_for_quoted_paths() {
        assert_eq!(
            interpret_override_value(OsStr::new("\" /opt/rsync  \"")),
            FallbackOverride::Explicit(OsString::from(" /opt/rsync  "))
        );
    }

    #[test]
    fn interpret_override_value_rejects_empty_quoted_values() {
        for value in ["\"\"", "'   '"] {
            assert_eq!(
                interpret_override_value(OsStr::new(value)),
                FallbackOverride::Disabled
            );
        }
    }

    #[test]
    fn fallback_env_constants_match_expected() {
        assert_eq!(CLIENT_FALLBACK_ENV, "OC_RSYNC_FALLBACK");
        assert_eq!(DAEMON_FALLBACK_ENV, "OC_RSYNC_DAEMON_FALLBACK");
        assert_eq!(DISABLE_FALLBACK_ENV, "OC_RSYNC_DISABLE_FALLBACK");
        assert_eq!(DAEMON_AUTO_DELEGATE_ENV, "OC_RSYNC_DAEMON_AUTO_DELEGATE");
    }

    #[test]
    fn disable_fallback_env_defaults_to_enabled() {
        let _guard = EnvVarGuard::remove(DISABLE_FALLBACK_ENV);
        assert!(!fallback_invocation_disabled());
    }

    #[test]
    fn disable_fallback_env_respects_truthy_values() {
        let _guard = EnvVarGuard::set(DISABLE_FALLBACK_ENV, "1");
        assert!(fallback_invocation_disabled());
        assert!(
            fallback_disabled_reason()
                .expect("reason present")
                .contains(DISABLE_FALLBACK_ENV)
        );
    }

    #[test]
    fn disable_fallback_env_accepts_falsey_values() {
        let _guard = EnvVarGuard::set(DISABLE_FALLBACK_ENV, "false");
        assert!(!fallback_invocation_disabled());
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
