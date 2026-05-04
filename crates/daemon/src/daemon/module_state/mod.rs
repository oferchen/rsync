//! Per-module configuration, runtime state, and connection management.
//!
//! This module defines the core types for rsync daemon module handling:
//! - [`ModuleDefinition`] - static configuration for a single module
//! - [`ModuleRuntime`] - live state pairing configuration with connection tracking
//! - [`ConnectionLimiter`] - cross-process connection limit enforcement via lock files
//! - Hostname resolution utilities for host-based access control
//!
//! upstream: loadparm.c - module parameters are loaded via `lp_load()` from
//! `rsyncd.conf`. clientserver.c - `rsync_module()` reads per-module settings
//! via `lp_*()` accessor functions at connection time.

mod auth;
mod connection_limiter;
mod definition;
mod hostname;
mod runtime;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub(crate) use auth::{AuthUser, UserAccessLevel};
pub(crate) use connection_limiter::{ConnectionLimiter, ConnectionLockGuard};
pub(crate) use definition::ModuleDefinition;
pub(crate) use hostname::module_peer_hostname;
pub(in crate::daemon) use hostname::resolve_peer_hostname;
pub(crate) use runtime::{ModuleConnectionError, ModuleRuntime};

#[cfg(test)]
pub(crate) use hostname::{clear_test_hostname_overrides, set_test_hostname_override};
#[cfg(test)]
use runtime::ModuleConnectionGuard;
#[cfg(test)]
pub(in crate::daemon) use test_support::TEST_CONFIG_CANDIDATES;
#[cfg(test)]
pub(crate) use test_support::{TEST_SECRETS_CANDIDATES, TEST_SECRETS_ENV, TestSecretsEnvOverride};
