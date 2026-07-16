use std::env;

/// Returns the default for `--protect-args` derived from `RSYNC_PROTECT_ARGS`.
///
/// Returns `None` when the variable is unset, `Some(false)` for the recognised
/// disable values (`0`, `no`, `false`, `off`, case-insensitive), and
/// `Some(true)` otherwise. Mirrors upstream rsync's environment handling so
/// CLI defaults match.
pub(crate) fn env_protect_args_default() -> Option<bool> {
    let value = env::var_os("RSYNC_PROTECT_ARGS")?;
    if value.is_empty() {
        return Some(true);
    }

    let normalized = value.to_string_lossy();
    let trimmed = normalized.trim();

    if trimmed.is_empty() {
        Some(true)
    } else if trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        Some(true)
    }
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

    #[test]
    fn env_protect_args_returns_none_when_unset() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::remove("RSYNC_PROTECT_ARGS");
        assert_eq!(env_protect_args_default(), None);
    }

    #[test]
    fn env_protect_args_returns_true_when_empty() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_true_for_1() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_false_for_0() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "0");
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

    #[test]
    fn env_protect_args_returns_true_for_yes() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "yes");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_true_for_true() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "true");
        assert_eq!(env_protect_args_default(), Some(true));
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
