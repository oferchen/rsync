use std::net::IpAddr;

use dns_lookup::lookup_addr;
#[cfg(not(test))]
use dns_lookup::lookup_host;

use super::ModuleDefinition;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;

/// Host string reported when reverse lookup is switched off, so no name was
/// ever sought.
///
/// upstream: clientserver.c:126 `const char undetermined_hostname[] =
/// "UNDETERMINED";`, selected at clientserver.c:1525 when
/// `lp_reverse_lookup(-1)` is false.
pub(in crate::daemon) const UNDETERMINED_HOSTNAME: &str = "UNDETERMINED";

/// Host string reported when a lookup WAS attempted and produced no usable
/// name - the address is the `0.0.0.0` sentinel, the reverse lookup failed, or
/// forward confirmation rejected the result.
///
/// upstream: clientname.c:34 `static const char default_name[] = "UNKNOWN";`,
/// returned by `client_name()` on the `0.0.0.0` short-circuit (clientname.c:114)
/// and on every lookup failure (clientname.c:127, :150).
pub(in crate::daemon) const UNKNOWN_HOSTNAME: &str = "UNKNOWN";

/// Renders the peer host the way every upstream daemon log escape and
/// environment variable renders it.
///
/// The two sentinels are NOT interchangeable and the distinction is the whole
/// point: `UNDETERMINED` means nobody asked, `UNKNOWN` means we asked and the
/// answer was unusable. Collapsing them - as a bare `Option<&str>` does - loses
/// the reason, which is why oc previously printed one lowercase `unknown` for
/// both, the bare IP in one log line, and an empty string in
/// `RSYNC_HOST_NAME`.
///
/// Access control consumes the same string through [`PeerHost`], which keeps
/// the resolved/sentinel distinction the ACL rules need.
///
/// upstream: clientserver.c:1525 `host = lp_reverse_lookup(-1) ?
/// client_name(addr) : undetermined_hostname;`
pub(in crate::daemon) fn peer_host_display(resolved: Option<&str>, reverse_lookup: bool) -> &str {
    match resolved {
        Some(name) => name,
        None if reverse_lookup => UNKNOWN_HOSTNAME,
        None => UNDETERMINED_HOSTNAME,
    }
}

/// The peer host as access control sees it: always a name to match against,
/// plus whether that name came from DNS or is one of upstream's two sentinels.
///
/// Upstream passes a never-empty string into `allow_access()`, so a sentinel is
/// matched like any other name - `match_hostname` bails only on `!host ||
/// !*host` (access.c:37-38), and clientname.c:93-95 documents `UNKNOWN` as
/// deliberately usable in a `hosts allow` line. Modelling the host as a bare
/// `Option<&str>` made both sentinels unmatchable, so `hosts allow =
/// UNDETERMINED` refused every peer.
///
/// The variant is still needed because oc keeps one rule upstream does not: a
/// hostname-based DENY rule fails closed when no name was resolved. Upstream
/// can rely on reverse DNS being available (it resolves before `daemon chroot`,
/// so the result is cached); oc resolves per connection after a process-wide
/// chroot, where a chroot without NSS configuration silently yields no name.
/// Keying that guard on the sentinel rather than on `Option::is_none` is what
/// lets the allow-side sentinels match without disarming it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PeerHost<'a> {
    /// A name that reverse DNS produced (and forward-confirmed, when the
    /// module's `forward lookup` is on).
    Resolved(&'a str),
    /// `UNDETERMINED` (no lookup was attempted) or `UNKNOWN` (one was, and
    /// produced nothing usable).
    Sentinel(&'a str),
}

impl<'a> PeerHost<'a> {
    /// Builds the host exactly as the display path builds it, so the string an
    /// ACL matches is the string the log line prints.
    pub(crate) fn new(resolved: Option<&'a str>, reverse_lookup: bool) -> Self {
        match resolved {
            Some(name) => Self::Resolved(name),
            None => Self::Sentinel(peer_host_display(None, reverse_lookup)),
        }
    }

    /// The name to match patterns against - never empty, mirroring upstream's
    /// `host` argument to `allow_access()`.
    pub(crate) const fn as_str(&self) -> &'a str {
        match self {
            Self::Resolved(name) | Self::Sentinel(name) => name,
        }
    }

    /// Whether the name came from DNS. Only the fail-closed deny guard may ask.
    pub(crate) const fn is_resolved(&self) -> bool {
        matches!(self, Self::Resolved(_))
    }
}

/// Resolves the peer's hostname for a module that requires hostname-based access control.
///
/// Returns `None` when hostname lookup is disabled, the module has no hostname-based
/// host patterns, or DNS resolution fails. Results are cached in `cache` to avoid
/// repeated DNS lookups for the same peer.
///
/// The module's `forward lookup` parameter (default true) controls whether the
/// reverse-DNS name is forward-confirmed: when on, its A/AAAA records must
/// include `peer_ip`, otherwise the peer is treated as having no resolved
/// hostname (fail-closed), so a spoofed PTR cannot satisfy a hostname
/// allow-rule.
///
/// upstream: clientserver.c - reverse DNS is performed when `hosts allow` or
/// `hosts deny` patterns contain hostnames. The `reverse lookup` parameter
/// controls whether reverse DNS is attempted at all; the `forward lookup`
/// parameter (access.c:49 `allow_forward_dns`, default True) forward-confirms
/// the result.
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
        *cache = Some(resolve_peer_hostname(peer_ip, module.forward_lookup));
    }

    cache.as_ref().and_then(|value| value.as_deref())
}

/// Performs a reverse DNS lookup for the given IP address.
///
/// The result is normalized by removing trailing dots and lowercasing, matching
/// upstream rsync's hostname normalization for `hosts allow`/`hosts deny` matching.
///
/// When `forward_lookup` is true (the upstream default), the PTR-derived name is
/// forward-confirmed before being returned: the name's A/AAAA records must
/// include `peer_ip`. A name that does not forward-resolve back to the peer -
/// a spoofed or stale PTR record - yields `None`, so hostname allow-rules
/// fail closed and a forged PTR cannot satisfy a `hosts allow = <name>` rule.
/// This mirrors upstream `clientname.c:check_name`, which replaces an
/// unconfirmed name with the "UNKNOWN" placeholder.
///
/// upstream: clientname.c:416 `check_name` forward-confirms the reverse-DNS
/// name against the peer address; access.c:49 gates forward DNS on the
/// `forward lookup` daemon parameter (default True, daemon-parm.h:197).
pub(in crate::daemon) fn resolve_peer_hostname(
    peer_ip: IpAddr,
    forward_lookup: bool,
) -> Option<String> {
    let name = reverse_lookup_name(peer_ip)?;
    if forward_lookup && !forward_confirms(&name, peer_ip) {
        return None;
    }
    Some(name)
}

/// Performs the PTR (reverse) lookup and normalizes the result.
fn reverse_lookup_name(peer_ip: IpAddr) -> Option<String> {
    #[cfg(test)]
    if let Some(mapped) = TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow().get(&peer_ip).cloned()) {
        return mapped.map(normalize_hostname_owned);
    }

    lookup_addr(&peer_ip).ok().map(normalize_hostname_owned)
}

/// Forward-resolves `name` and returns whether `peer_ip` is among its A/AAAA
/// records. A forward lookup that fails (name does not resolve) returns false,
/// matching upstream `check_name`'s fail-closed behaviour on `getaddrinfo`
/// error (clientname.c:437-441).
fn forward_confirms(name: &str, peer_ip: IpAddr) -> bool {
    forward_resolve(name)
        .into_iter()
        .any(|addr| addr == peer_ip)
}

/// Forward-resolves `name` to its A/AAAA records.
///
/// This is the single forward-DNS seam shared by two callers: reverse-name
/// forward-confirmation (`forward_confirms`) and the forward resolution of
/// config-specified `hosts allow`/`hosts deny` hostname tokens
/// (`HostPattern::forward_resolve_matches`). The lookup key is normalized so
/// both callers agree on casing and trailing dots.
///
/// A name that does not resolve yields an empty vector, so callers fail
/// closed - mirroring upstream `access.c:57-58`, where a NULL `gethostbyname`
/// result produces no match.
///
/// Tests inject deterministic results via `TEST_FORWARD_OVERRIDES`, keeping
/// the daemon's access-control logic hermetic and free of real DNS traffic.
pub(in crate::daemon) fn forward_resolve(name: &str) -> Vec<IpAddr> {
    let key = normalize_hostname_owned(name.to_owned());
    resolve_forward_key(&key)
}

/// Test-build forward resolver: consults the injected override table and
/// returns an empty set for unknown names, keeping access-control tests
/// hermetic and free of real DNS traffic.
#[cfg(test)]
fn resolve_forward_key(key: &str) -> Vec<IpAddr> {
    TEST_FORWARD_OVERRIDES
        .with(|map| map.borrow().get(key).cloned())
        .unwrap_or_default()
}

/// Production forward resolver: queries the system resolver, returning an
/// empty set on error so callers fail closed.
#[cfg(not(test))]
fn resolve_forward_key(key: &str) -> Vec<IpAddr> {
    lookup_host(key)
        .map(|addrs| addrs.collect())
        .unwrap_or_default()
}

/// Tests whether the connecting client `host` belongs to the named `netgroup`.
///
/// This is the daemon-side seam for `@netgroup` tokens in `hosts allow`/`hosts
/// deny`. Production builds delegate to [`metadata::host_in_netgroup`], which
/// performs a real `innetgr` lookup on platforms with a netgroup database
/// (glibc, macOS, the BSDs) and is a no-op returning `false` on musl/Windows.
/// Tests inject deterministic membership via `TEST_NETGROUP_MEMBERS`, keeping
/// access-control tests hermetic and free of a real netgroup database.
///
/// upstream: access.c:42 `innetgr(tok + 1, host, NULL, NULL)` - only the
/// netgroup's host field is consulted (user and domain are NULL).
#[cfg(not(test))]
pub(in crate::daemon) fn netgroup_contains(netgroup: &str, host: &str) -> bool {
    metadata::host_in_netgroup(netgroup, host)
}

/// Test-build netgroup resolver: consults the injected membership table and
/// treats any unknown netgroup as empty, so a `@netgroup` token yields a
/// deterministic non-match without a real netgroup database.
#[cfg(test)]
pub(in crate::daemon) fn netgroup_contains(netgroup: &str, host: &str) -> bool {
    TEST_NETGROUP_MEMBERS.with(|map| {
        map.borrow()
            .get(netgroup)
            .is_some_and(|hosts| hosts.iter().any(|member| member == host))
    })
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
    pub(in crate::daemon) static TEST_FORWARD_OVERRIDES: RefCell<HashMap<String, Vec<IpAddr>>> =
        RefCell::new(HashMap::new());
    pub(in crate::daemon) static TEST_NETGROUP_MEMBERS: RefCell<HashMap<String, Vec<String>>> =
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

/// Sets a test override for the forward (A/AAAA) resolution of `name`.
///
/// The normalized `name` resolves to `addrs`; forward-confirmation succeeds
/// only when the peer IP is among them. A name with no override forward-
/// resolves to an empty set, so confirmation fails closed.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn set_test_forward_override(name: &str, addrs: &[IpAddr]) {
    let normalized = normalize_hostname_owned(name.to_owned());
    TEST_FORWARD_OVERRIDES.with(|map| {
        map.borrow_mut().insert(normalized, addrs.to_vec());
    });
}

/// Declares that `hosts` are members of `netgroup` for the duration of a test.
///
/// The daemon's `@netgroup` matching consults this table via
/// [`netgroup_contains`] in test builds, so access-control tests are hermetic
/// and never touch a real netgroup database.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn set_test_netgroup_members(netgroup: &str, hosts: &[&str]) {
    TEST_NETGROUP_MEMBERS.with(|map| {
        map.borrow_mut().insert(
            netgroup.to_owned(),
            hosts.iter().map(|host| (*host).to_owned()).collect(),
        );
    });
}

/// Clears all test hostname resolution overrides (reverse, forward, netgroup).
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn clear_test_hostname_overrides() {
    TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow_mut().clear());
    TEST_FORWARD_OVERRIDES.with(|map| map.borrow_mut().clear());
    TEST_NETGROUP_MEMBERS.with(|map| map.borrow_mut().clear());
}
