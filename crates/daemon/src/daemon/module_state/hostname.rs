use std::net::IpAddr;

use dns_lookup::lookup_addr;

use super::ModuleDefinition;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;

/// Resolves the peer's hostname for a module that requires hostname-based access control.
///
/// Returns `None` when hostname lookup is disabled, the module has no hostname-based
/// host patterns, or DNS resolution fails. Results are cached in `cache` to avoid
/// repeated DNS lookups for the same peer.
///
/// upstream: clientserver.c - reverse DNS is performed when `hosts allow` or
/// `hosts deny` patterns contain hostnames. The `reverse lookup` global parameter
/// controls whether reverse DNS is attempted at all.
pub(crate) fn module_peer_hostname<'a>(
    module: &ModuleDefinition,
    cache: &'a mut Option<Option<String>>,
    peer_ip: IpAddr,
    allow_lookup: bool,
) -> Option<&'a str> {
    if !allow_lookup || !module.requires_hostname_lookup() {
        return None;
    }

    if cache.is_none() {
        *cache = Some(resolve_peer_hostname(peer_ip));
    }

    cache.as_ref().and_then(|value| value.as_deref())
}

/// Performs a reverse DNS lookup for the given IP address.
///
/// The result is normalized by removing trailing dots and lowercasing, matching
/// upstream rsync's hostname normalization for `hosts allow`/`hosts deny` matching.
pub(in crate::daemon) fn resolve_peer_hostname(peer_ip: IpAddr) -> Option<String> {
    #[cfg(test)]
    if let Some(mapped) = TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow().get(&peer_ip).cloned()) {
        return mapped.map(normalize_hostname_owned);
    }

    lookup_addr(&peer_ip).ok().map(normalize_hostname_owned)
}

/// Normalizes a hostname by removing trailing dots and lowercasing.
pub(super) fn normalize_hostname_owned(mut name: String) -> String {
    if name.ends_with('.') {
        name.pop();
    }
    name.make_ascii_lowercase();
    name
}

#[cfg(test)]
thread_local! {
    pub(in crate::daemon) static TEST_HOSTNAME_OVERRIDES: RefCell<HashMap<IpAddr, Option<String>>> =
        RefCell::new(HashMap::new());
}

/// Sets a test override for hostname resolution of the given address.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn set_test_hostname_override(addr: IpAddr, hostname: Option<&str>) {
    TEST_HOSTNAME_OVERRIDES.with(|map| {
        map.borrow_mut()
            .insert(addr, hostname.map(|value| value.to_owned()));
    });
}

/// Clears all test hostname resolution overrides.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn clear_test_hostname_overrides() {
    TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow_mut().clear());
}
