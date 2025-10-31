#![cfg(test)]

//! Shared helpers for manipulating environment variables within daemon tests.
//! The helpers centralise the unsafe interactions with `std::env` so individual
//! tests can remain focused on their specific assertions while ensuring the
//! environment is restored even when panics occur.

use std::env;
use std::ffi::{OsStr, OsString};
use std::sync::Mutex;

/// Global mutex guarding environment mutations performed by daemon tests.
///
/// Tests in this crate adjust environment variables such as
/// `OC_RSYNC_FALLBACK` and `RSYNC_PROXY`. Acquiring the mutex before applying
/// overrides ensures the environment remains consistent even when tests run in
/// parallel.
pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Scoped helper that applies an environment change and restores the previous
/// value when dropped.
#[allow(unsafe_code)]
#[derive(Debug)]
pub(crate) struct EnvGuard {
    key: OsString,
    previous: Option<OsString>,
}

impl EnvGuard {
    /// Sets `key` to `value` for the duration of the guard.
    #[allow(dead_code, unsafe_code)]
    pub(crate) fn set(key: &'static str, value: &OsStr) -> Self {
        let key_os = OsString::from(key);
        let previous = env::var_os(&key_os);
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
    pub(crate) fn remove(key: &'static str) -> Self {
        let key_os = OsString::from(key);
        let previous = env::var_os(&key_os);
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
