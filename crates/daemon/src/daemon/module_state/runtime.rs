use std::io;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::{ConnectionLimiter, ConnectionLockGuard, ModuleDefinition};

/// Live state for a module, pairing its static definition with runtime connection tracking.
///
/// upstream: clientserver.c - upstream rsync maintains a global connection count
/// per module via `lp_lock_file()`. Our `AtomicU32` tracks in-process counts
/// while the optional `ConnectionLimiter` coordinates across daemon processes.
pub(crate) struct ModuleRuntime {
    pub(crate) definition: ModuleDefinition,
    pub(crate) active_connections: AtomicU32,
    pub(crate) connection_limiter: Option<Arc<ConnectionLimiter>>,
}

/// Error returned when a module connection cannot be established.
#[derive(Debug)]
pub(crate) enum ModuleConnectionError {
    /// The module's connection limit has been reached.
    Limit(NonZeroU32),
    /// An I/O error occurred while managing connection state.
    Io(io::Error),
}

impl ModuleConnectionError {
    /// Creates an `Io` variant from the given error.
    pub(in crate::daemon) const fn io(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<io::Error> for ModuleConnectionError {
    fn from(error: io::Error) -> Self {
        ModuleConnectionError::Io(error)
    }
}

impl ModuleRuntime {
    /// Creates a new runtime with the given definition and optional connection limiter.
    pub(in crate::daemon) const fn new(
        definition: ModuleDefinition,
        connection_limiter: Option<Arc<ConnectionLimiter>>,
    ) -> Self {
        Self {
            definition,
            active_connections: AtomicU32::new(0),
            connection_limiter,
        }
    }

    /// Attempts to acquire a connection slot, respecting max_connections limits.
    pub(in crate::daemon) fn try_acquire_connection(
        &self,
    ) -> Result<ModuleConnectionGuard<'_>, ModuleConnectionError> {
        if let Some(limit) = self.definition.max_connections() {
            if let Some(limiter) = &self.connection_limiter {
                match limiter.acquire(&self.definition.name, limit) {
                    Ok(lock_guard) => {
                        self.acquire_local_slot(limit)?;
                        return Ok(ModuleConnectionGuard::limited(self, Some(lock_guard)));
                    }
                    Err(error) => return Err(error),
                }
            }

            self.acquire_local_slot(limit)?;
            Ok(ModuleConnectionGuard::limited(self, None))
        } else {
            Ok(ModuleConnectionGuard::unlimited())
        }
    }

    /// Atomically acquires a local connection slot using compare-and-swap.
    fn acquire_local_slot(&self, limit: NonZeroU32) -> Result<(), ModuleConnectionError> {
        let limit_value = limit.get();
        let mut current = self.active_connections.load(Ordering::Acquire);
        loop {
            if current >= limit_value {
                return Err(ModuleConnectionError::Limit(limit));
            }

            match self.active_connections.compare_exchange(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(updated) => current = updated,
            }
        }
    }

    /// Releases a connection slot, decrementing the active count.
    pub(in crate::daemon) fn release(&self) {
        if self.definition.max_connections().is_some() {
            self.active_connections.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl From<ModuleDefinition> for ModuleRuntime {
    fn from(definition: ModuleDefinition) -> Self {
        Self::new(definition, None)
    }
}

impl std::ops::Deref for ModuleRuntime {
    type Target = ModuleDefinition;

    fn deref(&self) -> &Self::Target {
        &self.definition
    }
}

/// RAII guard that releases a module connection slot on drop.
pub(in crate::daemon) struct ModuleConnectionGuard<'a> {
    pub(in crate::daemon) module: Option<&'a ModuleRuntime>,
    pub(in crate::daemon) lock_guard: Option<ConnectionLockGuard>,
}

impl<'a> ModuleConnectionGuard<'a> {
    /// Creates a guard for a connection-limited module.
    pub(in crate::daemon) const fn limited(
        module: &'a ModuleRuntime,
        lock_guard: Option<ConnectionLockGuard>,
    ) -> Self {
        Self {
            module: Some(module),
            lock_guard,
        }
    }

    /// Creates a guard for a module with no connection limit.
    pub(in crate::daemon) const fn unlimited() -> Self {
        Self {
            module: None,
            lock_guard: None,
        }
    }
}

impl<'a> Drop for ModuleConnectionGuard<'a> {
    fn drop(&mut self) {
        if let Some(module) = self.module.take() {
            module.release();
        }

        self.lock_guard.take();
    }
}
