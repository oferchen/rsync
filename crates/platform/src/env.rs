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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn set_restores_on_drop() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_SET";

        // Ensure clean state.
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

        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, "original");
        }

        {
            let _guard = EnvGuard::remove(key);
            assert!(env::var_os(key).is_none());
        }

        assert_eq!(env::var(key).unwrap(), "original");

        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }
    }

    #[test]
    fn set_overwrites_existing() {
        let _lock = TEST_LOCK.lock().unwrap();
        let key = "PLATFORM_ENV_TEST_OVERWRITE";

        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, "old_value");
        }

        {
            let _guard = EnvGuard::set(key, OsStr::new("new_value"));
            assert_eq!(env::var(key).unwrap(), "new_value");
        }

        assert_eq!(env::var(key).unwrap(), "old_value");

        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }
    }
}
