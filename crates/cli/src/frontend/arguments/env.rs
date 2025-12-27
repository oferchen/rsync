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
    use std::sync::Mutex;

    // Use a mutex to ensure env var tests don't interfere with each other
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env_var<F, R>(key: &str, value: Option<&str>, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        let _guard = ENV_LOCK.lock().unwrap();
        let previous = env::var_os(key);

        // SAFETY: We hold a mutex lock to ensure only one test modifies env vars at a time
        unsafe {
            if let Some(v) = value {
                env::set_var(key, v);
            } else {
                env::remove_var(key);
            }
        }

        let result = f();

        // SAFETY: We hold a mutex lock to ensure only one test modifies env vars at a time
        unsafe {
            if let Some(prev) = previous {
                env::set_var(key, prev);
            } else {
                env::remove_var(key);
            }
        }

        result
    }

    #[test]
    fn env_protect_args_returns_none_when_unset() {
        with_env_var("RSYNC_PROTECT_ARGS", None, || {
            assert_eq!(env_protect_args_default(), None);
        });
    }

    #[test]
    fn env_protect_args_returns_true_when_empty() {
        with_env_var("RSYNC_PROTECT_ARGS", Some(""), || {
            assert_eq!(env_protect_args_default(), Some(true));
        });
    }

    #[test]
    fn env_protect_args_returns_true_for_1() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("1"), || {
            assert_eq!(env_protect_args_default(), Some(true));
        });
    }

    #[test]
    fn env_protect_args_returns_false_for_0() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("0"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_returns_false_for_no() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("no"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_returns_false_for_false() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("false"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_returns_false_for_off() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("off"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_case_insensitive_no() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("NO"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_case_insensitive_false() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("FALSE"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_case_insensitive_off() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("OFF"), || {
            assert_eq!(env_protect_args_default(), Some(false));
        });
    }

    #[test]
    fn env_protect_args_returns_true_for_yes() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("yes"), || {
            assert_eq!(env_protect_args_default(), Some(true));
        });
    }

    #[test]
    fn env_protect_args_returns_true_for_true() {
        with_env_var("RSYNC_PROTECT_ARGS", Some("true"), || {
            assert_eq!(env_protect_args_default(), Some(true));
        });
    }
}
