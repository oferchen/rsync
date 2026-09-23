//! Environment variable manipulation with RAII restoration.
//!
//! In Rust 2024 edition, `std::env::set_var` and `std::env::remove_var` are
//! unsafe because they are not thread-safe. This module provides a safe
//! `EnvGuard` that wraps these calls with `#[allow(unsafe_code)]` and
//! automatically restores the previous value on drop.
//!
//! # Thread Safety
//!
//! Callers must ensure no concurrent environment mutations. Use a global
//! mutex (e.g., `ENV_LOCK`) to serialize environment changes in tests.

use std::env;
use std::ffi::{OsStr, OsString};

/// Scoped helper that applies an environment change and restores the previous
/// value when dropped.
#[derive(Debug)]
pub struct EnvGuard {
    key: OsString,
    previous: Option<OsString>,
}

impl EnvGuard {
    /// Sets `key` to `value` for the duration of the guard.
    #[allow(unsafe_code)]
    pub fn set(key: &'static str, value: &OsStr) -> Self {
        let key_os = OsString::from(key);
        let previous = env::var_os(&key_os);
        // SAFETY: Caller must ensure no concurrent environment mutations.
        unsafe {
            env::set_var(&key_os, value);
        }
        Self {
            key: key_os,
            previous,
        }
    }

    /// Removes `key` for the duration of the guard.
    #[allow(unsafe_code)]
    pub fn remove(key: &'static str) -> Self {
        let key_os = OsString::from(key);
        let previous = env::var_os(&key_os);
        // SAFETY: Caller must ensure no concurrent environment mutations.
        unsafe {
            env::remove_var(&key_os);
        }
        Self {
            key: key_os,
            previous,
        }
    }
}

#[allow(unsafe_code)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: Restoring environment state during drop. The mutex guard
        // protecting the caller should still be held.
        if let Some(ref value) = self.previous {
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

/// The local host's name, as `gethostname(2)` reports it.
///
/// `None` when the kernel cannot supply a name or it is not valid UTF-8.
/// Callers that mirror OpenSSH's client percent expansion consume this for
/// the `%l`/`%L` tokens (openssh/ssh.c:1421 `gethostname()`); `platform`
/// owns the syscall per the unsafe-code policy, via the `nix` safe wrapper.
#[cfg(unix)]
#[must_use]
pub fn hostname() -> Option<String> {
    nix::unistd::gethostname().ok()?.into_string().ok()
}

/// Windows counterpart of [`hostname`]: the `COMPUTERNAME` environment
/// variable, the conventional spelling of the local machine name there.
#[cfg(windows)]
#[must_use]
pub fn hostname() -> Option<String> {
    env::var("COMPUTERNAME").ok().filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn set_restores_on_drop() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_SET";

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }

        {
            let _guard = EnvGuard::set(key, OsStr::new("test_value"));
            assert_eq!(env::var(key).unwrap(), "test_value");
        }

        assert!(env::var_os(key).is_none());
    }

    #[test]
    fn remove_restores_on_drop() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_REMOVE";

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, "original");
        }

        {
            let _guard = EnvGuard::remove(key);
            assert!(env::var_os(key).is_none());
        }

        assert_eq!(env::var(key).unwrap(), "original");

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }
    }

    #[test]
    fn set_overwrites_existing() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_OVERWRITE";

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, "old_value");
        }

        {
            let _guard = EnvGuard::set(key, OsStr::new("new_value"));
            assert_eq!(env::var(key).unwrap(), "new_value");
        }

        assert_eq!(env::var(key).unwrap(), "old_value");

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }
    }

    #[test]
    fn nested_guards_restore_in_reverse_order() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_NESTED";

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }

        {
            let _outer = EnvGuard::set(key, OsStr::new("outer"));
            assert_eq!(env::var(key).unwrap(), "outer");
            {
                let _inner = EnvGuard::set(key, OsStr::new("inner"));
                assert_eq!(env::var(key).unwrap(), "inner");
            }
            assert_eq!(env::var(key).unwrap(), "outer");
        }

        assert!(env::var_os(key).is_none());
    }

    #[test]
    fn remove_nonexistent_key_restores_to_absent() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_REMOVE_NONEXIST";

        // SAFETY: the surrounding test holds `TEST_LOCK`, serialising every
        // environment mutation in this module so no other thread can race.
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }

        {
            let _guard = EnvGuard::remove(key);
            assert!(env::var_os(key).is_none());
        }

        assert!(env::var_os(key).is_none());
    }

    #[test]
    fn hostname_is_nonempty_when_available() {
        // Every supported CI host has a hostname; a `Some` must never be
        // empty, since consumers derive `%L` by splitting at the first dot.
        if let Some(name) = super::hostname() {
            assert!(!name.is_empty());
        }
    }
}
