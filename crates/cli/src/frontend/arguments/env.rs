use std::env;

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

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Scoped helper that sets or removes an environment variable and restores
    /// the previous value when dropped.
    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
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
        let _guard = EnvGuard::remove("RSYNC_PROTECT_ARGS");
        assert_eq!(env_protect_args_default(), None);
    }

    #[test]
    fn env_protect_args_returns_true_when_empty() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_true_for_1() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "1");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_false_for_0() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "0");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_false_for_no() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "no");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_false_for_false() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "false");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_false_for_off() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "off");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_case_insensitive_no() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "NO");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_case_insensitive_false() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "FALSE");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_case_insensitive_off() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "OFF");
        assert_eq!(env_protect_args_default(), Some(false));
    }

    #[test]
    fn env_protect_args_returns_true_for_yes() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "yes");
        assert_eq!(env_protect_args_default(), Some(true));
    }

    #[test]
    fn env_protect_args_returns_true_for_true() {
        let _guard = EnvGuard::set("RSYNC_PROTECT_ARGS", "true");
        assert_eq!(env_protect_args_default(), Some(true));
    }
}
