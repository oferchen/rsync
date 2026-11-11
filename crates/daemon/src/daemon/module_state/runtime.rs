use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::DaemonError;

use super::{ConnectionLimiter, ConnectionLockGuard, MaxConnections, ModuleDefinition};

/// Opens the daemon-wide `lock file`, if one is configured, and pairs every
/// module with the limiter enforcing its `max connections`.
///
/// This is the single entry point every daemon entry point builds its module
/// table through - the standalone accept loop, the async listener, the
/// `--server` stdio session and the inetd session - so a module's
/// `lock file` / `max connections` pair is honoured identically on all of
/// them.
///
/// The limiter is returned alongside the runtimes because callers that rebuild
/// their module table on SIGHUP need to hand the same daemon-wide handle to
/// [`build_module_runtimes`] again.
///
/// upstream: clientserver.c:791 calls `claim_connection()` from
/// `rsync_module()` - the one per-module entry every daemon connection reaches,
/// whether it arrived over a listening socket, inetd or a remote shell. The
/// `am_daemon > 0` test just above it guards only the "allowed access" log
/// line, not the claim, so the cap is deliberately not standalone-only.
pub(in crate::daemon) fn build_module_runtimes_with_lock_file(
    definitions: Vec<ModuleDefinition>,
    lock_file: Option<PathBuf>,
) -> Result<(Vec<ModuleRuntime>, Option<Arc<ConnectionLimiter>>), DaemonError> {
    let global_limiter = lock_file.map(|path| Arc::new(ConnectionLimiter::open(path)));
    let runtimes = build_module_runtimes(definitions, &global_limiter)?;
    Ok((runtimes, global_limiter))
}

/// Pairs each module definition with the connection limiter that enforces its
/// `max connections` cap, honouring a per-module `lock file` override.
///
/// A module that sets its own `lock file` gets a limiter bound to that path;
/// modules without an override share `global_limiter` (the daemon-wide lock
/// file). Distinct override paths are opened once and shared via the cache so
/// two modules naming the same file coordinate through one handle.
///
/// upstream: clientserver.c:791 calls `claim_connection(lp_lock_file(i),
/// lp_max_connections(i))`. The lock file is a P_LOCAL parameter, so each module
/// claims its slot in the file named by its own (inherited or overridden) value.
pub(in crate::daemon) fn build_module_runtimes(
    definitions: Vec<ModuleDefinition>,
    global_limiter: &Option<Arc<ConnectionLimiter>>,
) -> Result<Vec<ModuleRuntime>, DaemonError> {
    let mut cache: HashMap<PathBuf, Arc<ConnectionLimiter>> = HashMap::new();
    let mut runtimes = Vec::with_capacity(definitions.len());
    for definition in definitions {
        let limiter = match definition.lock_file.clone() {
            Some(path) => {
                let limiter = match cache.get(&path) {
                    Some(existing) => Arc::clone(existing),
                    None => {
                        let created = Arc::new(ConnectionLimiter::open(path.clone()));
                        cache.insert(path, Arc::clone(&created));
                        created
                    }
                };
                Some(limiter)
            }
            None => global_limiter.clone(),
        };
        runtimes.push(ModuleRuntime::new(definition, limiter));
    }
    Ok(runtimes)
}

/// Live state for a module, pairing its static definition with the limiter
/// that enforces its `max connections`.
///
/// upstream: connection.c:26-49 `claim_connection()` keeps no counter; the slot
/// locks in `lp_lock_file()` are its only state, so they are the only state
/// here too.
pub(crate) struct ModuleRuntime {
    pub(crate) definition: ModuleDefinition,
    pub(crate) connection_limiter: Option<Arc<ConnectionLimiter>>,
}

/// Error returned when a module connection cannot be established.
#[derive(Debug)]
pub(crate) enum ModuleConnectionError {
    /// The module's connection limit has been reached.
    ///
    /// `configured` carries the configured number verbatim, so a module
    /// disabled with a negative `max connections` reports its minus sign
    /// exactly as upstream's `%d` does. `active` is the number of slots the lock
    /// file showed held when it refused.
    Limit { configured: i32, active: u32 },
    /// An I/O error occurred while managing connection state.
    Io(io::Error),
}

impl ModuleConnectionError {
    /// Creates an `Io` variant from the given error.
    pub(in crate::daemon) const fn io(error: io::Error) -> Self {
        Self::Io(error)
    }

    /// Creates a `Limit` refusal from the mechanism that observed `active`
    /// connections while enforcing `configured`.
    pub(in crate::daemon) const fn limit(configured: MaxConnections, active: u32) -> Self {
        Self::Limit {
            configured: configured.display_value(),
            active,
        }
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
            connection_limiter,
        }
    }

    /// Attempts to acquire a connection slot, honouring the module's
    /// `max connections` setting.
    ///
    /// The step order mirrors upstream: connection.c:26-49 `claim_connection()`
    /// returns success for a zero limit *before* the lock file is opened, opens
    /// the lock file for every other value (so an unopenable file reports the
    /// open failure rather than the cap), and only then scans
    /// `[0, max_connections)`. A negative limit runs no iterations, so it
    /// consumes no slot: `ConnectionLimiter::acquire` refuses it after closing
    /// the descriptor, and without a limiter the `Disabled` arm below refuses
    /// it.
    pub(in crate::daemon) fn try_acquire_connection(
        &self,
    ) -> Result<ModuleConnectionGuard, ModuleConnectionError> {
        let configured = self.definition.max_connections();
        if matches!(configured, MaxConnections::Unlimited) {
            return Ok(ModuleConnectionGuard::unlimited());
        }

        let lock_guard = match &self.connection_limiter {
            Some(limiter) => Some(limiter.acquire(&self.definition.name, configured)?),
            None => None,
        };

        match configured {
            // A disabling limit holds no slots, so nothing is active: the
            // module is refused by its own configuration, not by capacity.
            MaxConnections::Disabled(_) => Err(ModuleConnectionError::limit(configured, 0)),
            _ => Ok(ModuleConnectionGuard { _slot: lock_guard }),
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

/// RAII guard that holds a module connection slot.
///
/// Dropping it closes the lock file, which releases the slot's range lock.
pub(in crate::daemon) struct ModuleConnectionGuard {
    _slot: Option<ConnectionLockGuard>,
}

impl ModuleConnectionGuard {
    /// Creates a guard for a module with no connection limit.
    pub(in crate::daemon) const fn unlimited() -> Self {
        Self { _slot: None }
    }
}
