use std::env;

/// Returns the default for `--protect-args` derived from `RSYNC_PROTECT_ARGS`.
///
/// Returns `None` when the variable is unset or empty (so the compile-time
/// default decides), and `Some(atoi(value) != 0)` when it is non-empty.
///
/// upstream: options.c:2139-2151 - `(arg = getenv("RSYNC_PROTECT_ARGS")) != NULL
/// && *arg` requires a non-empty value; an empty string falls through to the
/// compile default (`RSYNC_USE_SECLUDED_ARGS`), which we model as `None` so the
/// caller applies its own default. A non-empty value maps to
/// `protect_args = atoi(arg) ? 1 : 0`, i.e. C `atoi` reads a leading base-10
/// integer, so `1`/`2`/`3x` enable it while `0`/`no`/`false`/`off`/`yes`/`true`
/// (all `atoi` 0) disable it.
pub(crate) fn env_protect_args_default() -> Option<bool> {
    let value = env::var_os("RSYNC_PROTECT_ARGS")?;
    if value.is_empty() {
        return None;
    }
    Some(atoi_leading(&value.to_string_lossy()) != 0)
}

/// Resolves `--secluded-args` and `--old-args` against `RSYNC_OLD_ARGS` and
/// `RSYNC_PROTECT_ARGS` in upstream's order, returning `(protect_args, old_args)`.
///
/// `RSYNC_OLD_ARGS` is read first, and only when neither `--old-args` nor
/// `--secluded-args` was given; a non-empty value pins secluded args off even
/// at level 0, so `RSYNC_PROTECT_ARGS` is then never read. An explicit
/// `--old-args` turns secluded args off too, unless `--secluded-args` was also
/// given, in which case both are kept for the config builder's conflict error.
///
/// upstream: options.c:2124-2152.
pub(crate) fn resolve_arg_protection(
    protect_args: Option<bool>,
    old_args: Option<u8>,
) -> (Option<bool>, Option<u8>) {
    let (protect_args, old_args) = match old_args {
        None if protect_args != Some(true) => match env_old_args_level() {
            Some(level) => (Some(false), (level > 0).then_some(level)),
            None => (protect_args, None),
        },
        Some(level) if level > 0 && protect_args != Some(true) => (Some(false), old_args),
        _ => (protect_args, old_args),
    };
    (protect_args.or_else(env_protect_args_default), old_args)
}

/// Returns the `--old-args` level `RSYNC_OLD_ARGS` sets, or `None` when the
/// variable is unset or empty. A set value that `atoi` reads as 0 is `Some(0)`.
///
/// The level is capped at 2, the highest state safe_arg distinguishes.
///
/// upstream: options.c:2125-2127 - `(arg = getenv("RSYNC_OLD_ARGS")) != NULL
/// && *arg` then `old_style_args = atoi(arg)`.
fn env_old_args_level() -> Option<u8> {
    let value = env::var_os("RSYNC_OLD_ARGS").filter(|value| !value.is_empty())?;
    Some(atoi_leading(&value.to_string_lossy()).clamp(0, 2) as u8)
}

/// Parses a leading base-10 integer like C's `atoi`, ignoring leading
/// whitespace and any trailing non-digit suffix. Returns 0 when no digits lead.
///
/// Shared leading-integer parser for the CLI's environment defaults:
/// `RSYNC_PROTECT_ARGS` (options.c:2107 `protect_args = atoi(arg) ? 1 : 0`) and
/// `RSYNC_OLD_ARGS` (options.c:2127 `old_style_args = atoi(arg)`, via
/// `env_old_args_level`).
///
/// upstream: options.c:2127 & 2107 call libc `atoi(3)` (stdlib.h); this mirrors
/// its optional-sign, leading-decimal parse.
pub(crate) fn atoi_leading(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut sign = 1i32;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        if bytes[i] == b'-' {
            sign = -1;
        }
        i += 1;
    }
    let mut value = 0i32;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((bytes[i] - b'0') as i32);
        i += 1;
    }
    sign * value
}

/// Returns the default `--iconv` value derived from `RSYNC_ICONV`.
///
/// Returns `Some(value)` when the variable is set and non-empty, `None`
/// otherwise. Mirrors upstream rsync's `options.c:1509-1510`
/// (`(arg = getenv("RSYNC_ICONV")) != NULL && *arg`), which seeds `iconv_opt`
/// from the environment when the option was not given on the command line.
pub(crate) fn env_iconv_default() -> Option<std::ffi::OsString> {
    env::var_os("RSYNC_ICONV").filter(|value| !value.is_empty())
}

/// Returns the default `--max-alloc` argument derived from `RSYNC_MAX_ALLOC`.
///
/// Returns `Some(value)` when the variable is set and non-empty, `None`
/// otherwise. Mirrors upstream rsync's `options.c:2103-2107`
/// (`max_alloc_arg = getenv("RSYNC_MAX_ALLOC"); if (max_alloc_arg &&
/// !*max_alloc_arg) max_alloc_arg = NULL`), which supplies the default cap when
/// `--max-alloc` was not given on the command line.
pub(crate) fn env_max_alloc_default() -> Option<std::ffi::OsString> {
    env::var_os("RSYNC_MAX_ALLOC").filter(|value| !value.is_empty())
}

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    /// Serializes environment mutations so parallel test threads do not race on
    /// the same process-global variable.
    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    /// Scoped helper that sets or removes an environment variable and restores
    /// the previous value when dropped.
    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            // SAFETY: callers hold `ENV_MUTEX`, so no other thread can call
            // `getenv`/`setenv` concurrently. `set_var` is unsafe in Rust 2024
            // only because of cross-thread races, which the mutex prevents.
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            // SAFETY: see `set` above; the mutex serialises every environment
            // mutation in this module.
            unsafe {
                env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: `Drop` runs at scope exit while the test still holds
            // `ENV_MUTEX`, so no concurrent reader/writer can race the
            // restoration call.
            if let Some(value) = self.previous.take() {
                unsafe {
                    env::set_var(self.key, value);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }

    /// upstream: options.c:2127 & 2107 call libc `atoi(3)` on the env value; it
    /// reads an optional sign plus leading decimal digits and stops at the first
    /// non-digit, so `12abc` -> 12 while `yes`/`true`/`abc`/`` -> 0. This pins
    /// the exact mapping both `RSYNC_PROTECT_ARGS` and `RSYNC_OLD_ARGS` reuse.
    #[test]
    fn atoi_leading_matches_c_atoi() {
        assert_eq!(atoi_leading("1"), 1);
        assert_eq!(atoi_leading("2"), 2);
        assert_eq!(atoi_leading("10"), 10);
        assert_eq!(atoi_leading("12abc"), 12);
        assert_eq!(atoi_leading("  2"), 2);
        assert_eq!(atoi_leading("2 "), 2);
        assert_eq!(atoi_leading("3x"), 3);
        assert_eq!(atoi_leading("-5"), -5);
        assert_eq!(atoi_leading("0"), 0);
        assert_eq!(atoi_leading("0x"), 0);
        assert_eq!(atoi_leading("0abc"), 0);
        assert_eq!(atoi_leading("abc"), 0);
        assert_eq!(atoi_leading("yes"), 0);
        assert_eq!(atoi_leading("true"), 0);
        assert_eq!(atoi_leading(""), 0);
    }

    #[test]
    fn env_protect_args_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_PROTECT_ARGS");
        assert_eq!(env_protect_args_default(), None);
    }

    // upstream: options.c:2142 - `*arg` requires a non-empty value; an empty
    // string falls through to the compile default, modelled here as `None`.
    #[test]
    fn env_protect_args_returns_none_when_empty() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "");
        assert_eq!(env_protect_args_default(), None);
    }

    #[test]
    fn env_protect_args_returns_true_for_1() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    // upstream: options.c:2143 - `atoi("2")` is non-zero, so protect_args = 1.
    #[test]
    fn env_protect_args_returns_true_for_2() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "2");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    // upstream: options.c:2143 - `atoi("3x")` reads the leading 3, so on.
    #[test]
    fn env_protect_args_returns_true_for_leading_int_suffix() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "3x");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_false_for_0() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "0");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:2143 - `atoi("0abc")` is 0, so protect_args = 0.
    #[test]
    fn env_protect_args_returns_false_for_zero_suffix() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "0abc");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_false_for_no() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "no");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_false_for_false() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "false");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_false_for_off() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "off");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:2143 - `atoi("yes")` is 0, so the word disables it.
    #[test]
    fn env_protect_args_returns_false_for_yes() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "yes");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:2143 - `atoi("true")` is 0, so off.
    #[test]
    fn env_protect_args_returns_false_for_true() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "true");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:2143 - `atoi("on")` is 0, so off.
    #[test]
    fn env_protect_args_returns_false_for_on() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "on");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:2143 - `atoi("enabled")` is 0, so off.
    #[test]
    fn env_protect_args_returns_false_for_enabled() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "enabled");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_case_insensitive_no() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "NO");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_case_insensitive_false() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "FALSE");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_case_insensitive_off() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "OFF");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:1509-1510 - RSYNC_ICONV seeds the default --iconv value.
    #[test]
    fn env_iconv_default_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_ICONV");
        assert_eq!(env_iconv_default(), None);
    }

    // upstream: options.c:1509-1510 - `*arg` requires a non-empty value.
    #[test]
    fn env_iconv_default_ignores_empty_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_ICONV", "");
        assert_eq!(env_iconv_default(), None);
    }

    // upstream: options.c:1509-1510 - a non-empty value becomes iconv_opt.
    #[test]
    fn env_iconv_default_returns_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_ICONV", "utf-8,latin1");
        assert_eq!(
            env_iconv_default(),
            Some(std::ffi::OsString::from("utf-8,latin1"))
        );
    }

    // upstream: options.c:2103-2107 - RSYNC_MAX_ALLOC seeds the default cap.
    #[test]
    fn env_max_alloc_default_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_MAX_ALLOC");
        assert_eq!(env_max_alloc_default(), None);
    }

    // upstream: options.c:2105-2106 - an empty value is treated as unset.
    #[test]
    fn env_max_alloc_default_ignores_empty_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_MAX_ALLOC", "");
        assert_eq!(env_max_alloc_default(), None);
    }

    // upstream: options.c:2103-2104 - a non-empty value becomes max_alloc_arg.
    #[test]
    fn env_max_alloc_default_returns_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_MAX_ALLOC", "2G");
        assert_eq!(
            env_max_alloc_default(),
            Some(std::ffi::OsString::from("2G"))
        );
    }

    fn protection_env(old: Option<&str>, protect: Option<&str>) -> [EnvGuard; 2] {
        let guard = |key, value: Option<&str>| match value {
            Some(value) => EnvGuard::set(key, value),
            None => EnvGuard::remove(key),
        };
        [
            guard("RSYNC_OLD_ARGS", old),
            guard("RSYNC_PROTECT_ARGS", protect),
        ]
    }

    /// upstream: options.c:2124-2129 runs before options.c:2139-2152, so a set
    /// `RSYNC_OLD_ARGS` pins secluded args off and `RSYNC_PROTECT_ARGS` is never
    /// read - even when `RSYNC_OLD_ARGS` is `0`. Reading `RSYNC_PROTECT_ARGS`
    /// first would send secluded args to a peer the user asked for old args.
    #[test]
    fn env_old_args_wins_over_env_protect_args() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env = protection_env(Some("1"), Some("1"));
        assert_eq!(resolve_arg_protection(None, None), (Some(false), Some(1)));
        let _env = protection_env(Some("0"), Some("1"));
        assert_eq!(resolve_arg_protection(None, None), (Some(false), None));
    }

    /// upstream: options.c:2130-2135 - an explicit `--old-args` forces
    /// `protect_args = 0` before the env default is consulted, so
    /// `RSYNC_PROTECT_ARGS=1` must not turn into a conflict with `--old-args`.
    #[test]
    fn explicit_old_args_suppresses_env_protect_args() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env = protection_env(None, Some("1"));
        assert_eq!(
            resolve_arg_protection(None, Some(2)),
            (Some(false), Some(2))
        );
    }

    /// upstream: options.c:2130-2134 - `--secluded-args --old-args` stays a
    /// conflict; both survive so the config builder can refuse the pair.
    #[test]
    fn explicit_pair_is_kept_for_the_conflict_check() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env = protection_env(Some("2"), None);
        assert_eq!(
            resolve_arg_protection(Some(true), Some(1)),
            (Some(true), Some(1))
        );
    }

    /// upstream: options.c:2125 - `protect_args <= 0` gates the env read, so an
    /// explicit `--secluded-args` ignores `RSYNC_OLD_ARGS` while
    /// `--no-secluded-args` still honours it.
    #[test]
    fn env_old_args_is_read_unless_secluded_args_was_given() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env = protection_env(Some("2"), None);
        assert_eq!(resolve_arg_protection(Some(true), None), (Some(true), None));
        assert_eq!(
            resolve_arg_protection(Some(false), None),
            (Some(false), Some(2))
        );
    }

    /// An explicit `--old-args`/`--no-old-args` level is never replaced by the
    /// env, and `--no-old-args` still lets `RSYNC_PROTECT_ARGS` decide.
    #[test]
    fn explicit_old_args_level_ignores_env_old_args() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env = protection_env(Some("2"), Some("1"));
        assert_eq!(
            resolve_arg_protection(None, Some(1)),
            (Some(false), Some(1))
        );
        assert_eq!(resolve_arg_protection(None, Some(0)), (Some(true), Some(0)));
    }

    /// upstream: options.c:2127 `old_style_args = atoi(arg)` - the level is an
    /// integer (2 disables all safe_arg escaping), and an empty value counts
    /// as unset, leaving `RSYNC_PROTECT_ARGS` in charge.
    #[test]
    fn env_old_args_level_follows_atoi() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _env = protection_env(Some("2"), None);
        assert_eq!(resolve_arg_protection(None, None), (Some(false), Some(2)));
        let _env = protection_env(Some("yes"), None);
        assert_eq!(resolve_arg_protection(None, None), (Some(false), None));
        let _env = protection_env(Some(""), Some("1"));
        assert_eq!(resolve_arg_protection(None, None), (Some(true), None));
        let _env = protection_env(None, None);
        assert_eq!(resolve_arg_protection(None, None), (None, None));
    }
}
