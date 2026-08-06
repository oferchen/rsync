use std::env;

/// Returns the default for `--protect-args` derived from `RSYNC_PROTECT_ARGS`.
///
/// Returns `None` when the variable is unset or empty (so the compile-time
/// default decides), and `Some(atoi(value) != 0)` when it is non-empty.
///
/// upstream: options.c:1985-1994 - `(arg = getenv("RSYNC_PROTECT_ARGS")) != NULL
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

/// Parses a leading base-10 integer like C's `atoi`, ignoring leading
/// whitespace and any trailing non-digit suffix. Returns 0 when no digits lead.
///
/// Kept consistent with the `RSYNC_OLD_ARGS` parser (`run.rs::atoi_leading`,
/// upstream: options.c:1971 `old_style_args = atoi(arg)`).
fn atoi_leading(s: &str) -> i32 {
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
/// otherwise. Mirrors upstream rsync's `options.c:1377-1378`
/// (`(arg = getenv("RSYNC_ICONV")) != NULL && *arg`), which seeds `iconv_opt`
/// from the environment when the option was not given on the command line.
pub(crate) fn env_iconv_default() -> Option<std::ffi::OsString> {
    env::var_os("RSYNC_ICONV").filter(|value| !value.is_empty())
}

/// Returns the default `--max-alloc` argument derived from `RSYNC_MAX_ALLOC`.
///
/// Returns `Some(value)` when the variable is set and non-empty, `None`
/// otherwise. Mirrors upstream rsync's `options.c:1954-1957`
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

    /// upstream: options.c:1988 - `atoi(arg) ? 1 : 0` uses C `atoi`, a leading
    /// base-10 parse. This pins the exact mapping oc must reproduce.
    #[test]
    fn atoi_leading_matches_c_atoi() {
        assert_eq!(atoi_leading("1"), 1);
        assert_eq!(atoi_leading("2"), 2);
        assert_eq!(atoi_leading("  2"), 2);
        assert_eq!(atoi_leading("2 "), 2);
        assert_eq!(atoi_leading("3x"), 3);
        assert_eq!(atoi_leading("0abc"), 0);
        assert_eq!(atoi_leading("yes"), 0);
        assert_eq!(atoi_leading(""), 0);
    }

    #[test]
    fn env_protect_args_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_PROTECT_ARGS");
        assert_eq!(env_protect_args_default(), None);
    }

    // upstream: options.c:1987 - `*arg` requires a non-empty value; an empty
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

    // upstream: options.c:1988 - `atoi("2")` is non-zero, so protect_args = 1.
    #[test]
    fn env_protect_args_returns_true_for_2() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "2");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    // upstream: options.c:1988 - `atoi("3x")` reads the leading 3, so on.
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

    // upstream: options.c:1988 - `atoi("0abc")` is 0, so protect_args = 0.
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

    // upstream: options.c:1988 - `atoi("yes")` is 0, so the word disables it
    // (the pre-fix parser wrongly treated any non-disable word as enabled).
    #[test]
    fn env_protect_args_returns_false_for_yes() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "yes");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:1988 - `atoi("true")` is 0, so off.
    #[test]
    fn env_protect_args_returns_false_for_true() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "true");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:1988 - `atoi("on")` is 0, so off.
    #[test]
    fn env_protect_args_returns_false_for_on() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "on");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    // upstream: options.c:1988 - `atoi("enabled")` is 0, so off.
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

    // upstream: options.c:1377-1378 - RSYNC_ICONV seeds the default --iconv value.
    #[test]
    fn env_iconv_default_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_ICONV");
        assert_eq!(env_iconv_default(), None);
    }

    // upstream: options.c:1377-1378 - `*arg` requires a non-empty value.
    #[test]
    fn env_iconv_default_ignores_empty_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_ICONV", "");
        assert_eq!(env_iconv_default(), None);
    }

    // upstream: options.c:1377-1378 - a non-empty value becomes iconv_opt.
    #[test]
    fn env_iconv_default_returns_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_ICONV", "utf-8,latin1");
        assert_eq!(
            env_iconv_default(),
            Some(std::ffi::OsString::from("utf-8,latin1"))
        );
    }

    // upstream: options.c:1954-1957 - RSYNC_MAX_ALLOC seeds the default cap.
    #[test]
    fn env_max_alloc_default_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_MAX_ALLOC");
        assert_eq!(env_max_alloc_default(), None);
    }

    // upstream: options.c:1956-1957 - an empty value is treated as unset.
    #[test]
    fn env_max_alloc_default_ignores_empty_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_MAX_ALLOC", "");
        assert_eq!(env_max_alloc_default(), None);
    }

    // upstream: options.c:1954-1955 - a non-empty value becomes max_alloc_arg.
    #[test]
    fn env_max_alloc_default_returns_value() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_MAX_ALLOC", "2G");
        assert_eq!(
            env_max_alloc_default(),
            Some(std::ffi::OsString::from("2G"))
        );
    }
}
