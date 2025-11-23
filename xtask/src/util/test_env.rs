use std::env;
use std::ffi::{OsStr, OsString};
use std::sync::{Mutex, MutexGuard, OnceLock};

fn env_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Scoped environment guard for test-only mutations.
#[cfg(test)]
pub(crate) struct EnvGuard {
    entries: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

#[cfg(test)]
impl EnvGuard {
    /// Acquires the global environment lock and prepares to track mutations.
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            _lock: env_lock().lock().expect("env lock poisoned"),
        }
    }

    /// Records the current value of an environment variable without mutating it.
    pub(crate) fn track(&mut self, key: &'static str) {
        if self.entries.iter().any(|(existing, _)| existing == &key) {
            return;
        }

        self.entries.push((key, env::var_os(key)));
    }

    /// Sets an environment variable, recording its previous value for restoration.
    #[allow(unsafe_code)]
    pub(crate) fn set(&mut self, key: &'static str, value: impl AsRef<OsStr>) {
        if self.entries.iter().all(|(existing, _)| existing != &key) {
            self.entries.push((key, env::var_os(key)));
        }

        // SAFETY: Environment mutations are serialized by the global lock, and the
        // previous value is restored when the guard drops.
        unsafe {
            env::set_var(key, value);
        }
    }

    /// Removes an environment variable while preserving its prior value.
    #[allow(unsafe_code)]
    pub(crate) fn remove(&mut self, key: &'static str) {
        if self.entries.iter().all(|(existing, _)| existing != &key) {
            self.entries.push((key, env::var_os(key)));
        }

        // SAFETY: Environment mutations are serialized by the global lock, and the
        // previous value is restored when the guard drops.
        unsafe {
            env::remove_var(key);
        }
    }
}

#[cfg(test)]
impl Drop for EnvGuard {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        for (key, previous) in self.entries.drain(..).rev() {
            if let Some(value) = previous {
                unsafe {
                    env::set_var(key, value);
                }
            } else {
                unsafe {
                    env::remove_var(key);
                }
            }
        }
    }
}
