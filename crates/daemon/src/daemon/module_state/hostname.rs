use std::net::{IpAddr, SocketAddr};

#[cfg(not(test))]
use dns_lookup::{AddrInfoHints, SockType, getaddrinfo};
use dns_lookup::{LookupError, LookupErrorKind, lookup_addr};

use super::ModuleDefinition;

#[cfg(test)]
use std::cell::RefCell;
#[cfg(test)]
use std::collections::HashMap;

/// Host string reported when reverse lookup is switched off, so no name was
/// ever sought.
///
/// upstream: clientserver.c:130 `const char undetermined_hostname[] =
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
/// the reason.
///
/// Access control consumes the same string through [`PeerHost`], which keeps
/// the resolved/sentinel distinction the ACL rules need.
///
/// upstream: clientserver.c:1535 `host = lp_reverse_lookup(-1) ?
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
/// !*host` (access.c:40-41), and clientname.c:93-95 documents `UNKNOWN` as
/// deliberately usable in a `hosts allow` line. Modelling the host as a bare
/// `Option<&str>` made both sentinels unmatchable, so `hosts allow =
/// UNDETERMINED` refused every peer.
///
/// Deviation from upstream: a hostname-based DENY rule fails closed when no
/// name was resolved, which is why the variant is still needed. Upstream can
/// rely on reverse DNS being available (it resolves before its per-connection
/// `daemon chroot`, so the result is cached); oc applies `daemon chroot` once at
/// startup, where a chroot without NSS configuration silently yields no name.
/// Keying that guard on the sentinel rather than on `Option::is_none` is what
/// lets the allow-side sentinels match without disarming it. The guard matches
/// this enum EXHAUSTIVELY rather than calling a helper, so a third variant is a
/// compile error there instead of silently defaulting to "allowed".
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
}

/// Resolves the peer's hostname for a module that requires hostname-based access control.
///
/// Returns `None` when hostname lookup is disabled, the module has no hostname-based
/// host patterns, or DNS resolution fails. Results are cached in `cache` to avoid
/// repeated DNS lookups for the same peer; a failure stays in the cache so the
/// caller can log it.
///
/// The reverse-DNS name is always forward-confirmed (see
/// [`resolve_peer_hostname`]); the module's `forward lookup` parameter does not
/// affect this step. It only governs forward resolution of hostname tokens in
/// the module's `hosts allow`/`hosts deny` lists.
///
/// upstream: clientserver.c - reverse DNS is performed when `hosts allow` or
/// `hosts deny` patterns contain hostnames. The `reverse lookup` parameter
/// controls whether reverse DNS is attempted at all; `forward lookup`
/// (access.c:60 `allow_forward_dns`) gates only the config-token lookup.
pub(crate) fn module_peer_hostname<'a>(
    module: &ModuleDefinition,
    cache: &'a mut Option<Result<String, NameLookupFailure>>,
    peer_ip: IpAddr,
    allow_lookup: bool,
) -> Option<&'a str> {
    if !allow_lookup || !module.requires_hostname_lookup() {
        return None;
    }

    cache
        .get_or_insert_with(|| resolve_peer_hostname(peer_ip))
        .as_deref()
        .ok()
}

/// Performs a reverse DNS lookup for the given IP address.
///
/// The name keeps the case the resolver returned, so `%h` and
/// `RSYNC_HOST_NAME` show it as upstream does (clientname.c `client_name`
/// never folds it). `hosts allow`/`hosts deny` stay case-insensitive because
/// the matcher folds the host, as upstream's `iwildmatch` does (access.c:61).
///
/// The PTR-derived name is forward-confirmed before being returned: the name's
/// A/AAAA records must include `peer_ip`. A name that does not forward-resolve
/// back to the peer - a spoofed or stale PTR record - yields `None`, so
/// hostname allow-rules fail closed and a forged PTR cannot satisfy a
/// `hosts allow = <name>` rule.
///
/// upstream: clientname.c:147-153 `client_name()` calls `check_name()`
/// (clientname.c:425) unconditionally, which replaces an unconfirmed name with
/// the "UNKNOWN" placeholder. No daemon parameter disables it.
pub(in crate::daemon) fn resolve_peer_hostname(
    peer_ip: IpAddr,
) -> Result<String, NameLookupFailure> {
    if peer_ip == IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED) {
        return Err(NameLookupFailure::Unaddressed);
    }
    let name = reverse_lookup_name(peer_ip).map_err(|reason| NameLookupFailure::Reverse {
        addr: peer_ip,
        reason,
    })?;
    let addrs = resolve_forward_key(&normalize_hostname_owned(name.clone())).map_err(|reason| {
        NameLookupFailure::Forward {
            name: name.clone(),
            reason,
        }
    })?;
    if addrs.is_empty() {
        return Err(NameLookupFailure::NoAddress { name });
    }
    if !addrs.iter().any(|addr| addr.ip() == peer_ip) {
        return Err(NameLookupFailure::NotKnown {
            addr: peer_ip,
            name,
        });
    }
    Ok(name)
}

/// Why `client_name()` fell back to `UNKNOWN`.
///
/// [`NameLookupFailure::log_line`] is the exact FLOG line upstream writes
/// before it returns the placeholder, when it writes one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum NameLookupFailure {
    /// The peer address is the `0.0.0.0` placeholder a daemon over a remote
    /// shell uses when no environment variable names the client.
    ///
    /// upstream: clientname.c:113-114 - returned without a lookup or a log line.
    Unaddressed,
    /// The peer string is not a numeric address.
    ///
    /// upstream: clientname.c:120-123 `malformed address %s: %s`.
    Malformed { text: String, reason: String },
    /// The PTR lookup failed.
    ///
    /// upstream: clientname.c:149-151 `name lookup failed for %s: %s`.
    Reverse { addr: IpAddr, reason: String },
    /// The forward lookup of the PTR name failed.
    ///
    /// upstream: clientname.c:436-440 `forward name lookup for %s failed: %s`.
    Forward { name: String, reason: String },
    /// The forward lookup succeeded with no addresses.
    ///
    /// upstream: clientname.c:450-454 `no known address for "%s": spoofed address?`.
    NoAddress { name: String },
    /// The forward lookup does not include the peer's address.
    ///
    /// upstream: clientname.c:455-460 `%s is not a known address for "%s": spoofed address?`.
    NotKnown { addr: IpAddr, name: String },
}

impl NameLookupFailure {
    /// The line upstream logs for this failure, or `None` when it logs nothing.
    pub(crate) fn log_line(&self) -> Option<String> {
        match self {
            Self::Unaddressed => None,
            Self::Malformed { text, reason } => Some(format!("malformed address {text}: {reason}")),
            Self::Reverse { addr, reason } => {
                Some(format!("name lookup failed for {addr}: {reason}"))
            }
            Self::Forward { name, reason } => {
                Some(format!("forward name lookup for {name} failed: {reason}"))
            }
            Self::NoAddress { name } => {
                Some(format!("no known address for \"{name}\": spoofed address?"))
            }
            Self::NotKnown { addr, name } => Some(format!(
                "{addr} is not a known address for \"{name}\": spoofed address?"
            )),
        }
    }
}

/// The failure `client_name()` reports for a peer string that does not parse
/// as a numeric address, or `None` when it parses.
///
/// upstream: clientname.c:116-123 - `getaddrinfo(ipaddr, NULL, &hint, ...)`
/// with `AI_NUMERICHOST` and `SOCK_STREAM`; on failure it logs `malformed
/// address %s: %s` with `gai_strerror()`.
pub(in crate::daemon) fn malformed_peer_address(text: &str) -> Option<NameLookupFailure> {
    let hints = dns_lookup::AddrInfoHints {
        flags: AI_NUMERICHOST,
        socktype: dns_lookup::SockType::Stream.into(),
        ..dns_lookup::AddrInfoHints::default()
    };
    dns_lookup::getaddrinfo(Some(text), None, Some(hints))
        .err()
        .map(|error| NameLookupFailure::Malformed {
            text: text.to_owned(),
            reason: gai_reason(&error),
        })
}

/// The C library's `AI_NUMERICHOST` hint, which `getaddrinfo` takes as an
/// `i32` flag on every platform `dns_lookup` supports.
#[cfg(not(windows))]
const AI_NUMERICHOST: i32 = libc::AI_NUMERICHOST;

/// WinSock's `AI_NUMERICHOST` hint (4, as in POSIX).
#[cfg(windows)]
const AI_NUMERICHOST: i32 = windows::Win32::Networking::WinSock::AI_NUMERICHOST as i32;

/// Performs the PTR (reverse) lookup and strips a trailing root dot.
///
/// The error is `gai_strerror()`'s text for the failure.
fn reverse_lookup_name(peer_ip: IpAddr) -> Result<String, String> {
    #[cfg(test)]
    if let Some(mapped) = TEST_HOSTNAME_OVERRIDES.with(|map| map.borrow().get(&peer_ip).cloned()) {
        return mapped
            .map(trim_root_dot)
            .ok_or_else(|| TEST_LOOKUP_FAILURE.to_owned());
    }

    lookup_addr(&peer_ip)
        .map(trim_root_dot)
        .map_err(|error| gai_reason(&error))
}

/// `gai_strerror()`'s text for a failed lookup, as upstream prints it.
///
/// `dns_lookup` renders a lookup error as `failed to lookup address
/// information: <gai_strerror text>`; `EAI_SYSTEM` carries the errno text
/// instead, where glibc's and the BSDs' `gai_strerror()` say `System error`.
fn gai_reason(error: &LookupError) -> String {
    if matches!(error.kind(), LookupErrorKind::System) {
        return "System error".to_owned();
    }
    let text = error.to_string();
    match text.strip_prefix("failed to lookup address information: ") {
        Some(reason) => reason.to_owned(),
        None => text,
    }
}

/// `gai_strerror(EAI_NONAME)` on glibc, the reason test lookups fail with.
#[cfg(test)]
const TEST_LOOKUP_FAILURE: &str = "Name or service not known";

/// Forward-resolves `name` to its A/AAAA records, each with the IPv6 scope id
/// the resolver attached (0 for none).
///
/// This is the single forward-DNS seam shared by two callers: reverse-name
/// forward-confirmation (`forward_confirms`) and the forward resolution of
/// config-specified `hosts allow`/`hosts deny` hostname tokens
/// (`HostPattern::forward_resolve_matches`). The lookup key is normalized so
/// both callers agree on casing and trailing dots.
///
/// `None` means the LOOKUP FAILED; `Some(addrs)` means it succeeded, possibly
/// with an empty or non-matching set. Upstream draws exactly this line and the
/// two sides differ: a NULL `gethostbyname` returns the caller's `deny` flag
/// (access.c:68-74), while a successful lookup whose records do not include the
/// peer falls out of the loop and returns 0 (access.c:77-87). Collapsing both
/// into "no records" - as an empty `Vec` did - makes an unresolvable `hosts
/// deny` token fail OPEN.
///
/// Tests inject deterministic results via `TEST_FORWARD_OVERRIDES`: a name with
/// an entry resolved successfully to those addresses (an empty slice models a
/// successful lookup with no records); a name with no entry models a failed
/// lookup.
pub(in crate::daemon) fn forward_resolve(name: &str) -> Option<Vec<SocketAddr>> {
    let key = normalize_hostname_owned(name.to_owned());
    resolve_forward_key(&key).ok()
}

/// Test-build forward resolver: consults the injected override table and
/// reports a FAILED lookup for unknown names, keeping access-control tests
/// hermetic and free of real DNS traffic.
#[cfg(test)]
fn resolve_forward_key(key: &str) -> Result<Vec<SocketAddr>, String> {
    TEST_FORWARD_OVERRIDES
        .with(|map| map.borrow().get(key).cloned())
        .ok_or_else(|| TEST_LOOKUP_FAILURE.to_owned())
}

/// Production forward resolver: queries the system resolver, reporting
/// `gai_strerror()`'s text when the lookup itself fails so callers can apply
/// upstream's per-list rule or log the failure.
///
/// upstream: access.c:76-82 `getaddrinfo(tok, NULL, &hints, &res0)` with
/// `PF_UNSPEC` and `SOCK_STREAM`; the full socket address is kept because
/// access.c:124-126 compares the IPv6 `sin6_scope_id`.
#[cfg(not(test))]
fn resolve_forward_key(key: &str) -> Result<Vec<SocketAddr>, String> {
    let hints = AddrInfoHints {
        socktype: SockType::Stream.into(),
        ..AddrInfoHints::default()
    };
    let records = getaddrinfo(Some(key), None, Some(hints)).map_err(|error| gai_reason(&error))?;
    Ok(records
        .filter_map(Result::ok)
        .map(|info| info.sockaddr)
        .collect())
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
/// upstream: access.c:49 `innetgr(tok + 1, host, NULL, NULL)` - only the
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

/// Removes the trailing root dot a resolver may append to a name.
fn trim_root_dot(mut name: String) -> String {
    if name.ends_with('.') {
        name.pop();
    }
    name
}

/// Normalizes a hostname key by removing trailing dots and lowercasing.
pub(super) fn normalize_hostname_owned(name: String) -> String {
    let mut name = trim_root_dot(name);
    name.make_ascii_lowercase();
    name
}

#[cfg(test)]
thread_local! {
    pub(in crate::daemon) static TEST_HOSTNAME_OVERRIDES: RefCell<HashMap<IpAddr, Option<String>>> =
        RefCell::new(HashMap::new());
    pub(in crate::daemon) static TEST_FORWARD_OVERRIDES: RefCell<HashMap<String, Vec<SocketAddr>>> =
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
/// only when the peer IP is among them. A name with no override models a
/// failed lookup, so confirmation fails closed.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn set_test_forward_override(name: &str, addrs: &[IpAddr]) {
    let records: Vec<SocketAddr> = addrs.iter().map(|&ip| SocketAddr::new(ip, 0)).collect();
    set_test_forward_records(name, &records);
}

/// Sets a test override for the forward resolution of `name` to full socket
/// addresses, so a record can carry an IPv6 scope id.
#[cfg(test)]
pub(crate) fn set_test_forward_records(name: &str, records: &[SocketAddr]) {
    let normalized = normalize_hostname_owned(name.to_owned());
    TEST_FORWARD_OVERRIDES.with(|map| {
        map.borrow_mut().insert(normalized, records.to_vec());
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
