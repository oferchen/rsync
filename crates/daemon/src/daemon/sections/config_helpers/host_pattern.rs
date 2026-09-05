/// A pattern for matching hosts in allow/deny lists.
///
/// Supports shell globs, CIDR notation for IP addresses, and hostname patterns.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum HostPattern {
    /// Matches any host ("*" or "all").
    Any,
    /// Matches an IPv4 network with CIDR prefix.
    Ipv4 { network: Ipv4Addr, prefix: u8 },
    /// Matches an IPv6 network with CIDR prefix.
    Ipv6 { network: Ipv6Addr, prefix: u8 },
    /// Matches by hostname pattern (exact, suffix, or wildcard).
    Hostname(HostnamePattern),
    /// Matches when the client's resolved hostname is a member of a netgroup
    /// (`@name` token). Holds the netgroup name (the text after `@`).
    Netgroup(String),
}

/// IP address family for filtering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AddressFamily {
    /// IPv4 address family.
    Ipv4,
    /// IPv6 address family.
    Ipv6,
}

impl AddressFamily {
    /// Returns the address family for a given IP address.
    const fn from_ip(addr: IpAddr) -> Self {
        match addr {
            IpAddr::V4(_) => Self::Ipv4,
            IpAddr::V6(_) => Self::Ipv6,
        }
    }

    /// Returns whether the given IP address belongs to this family.
    const fn matches(self, addr: IpAddr) -> bool {
        matches!(
            (self, addr),
            (Self::Ipv4, IpAddr::V4(_)) | (Self::Ipv6, IpAddr::V6(_))
        )
    }
}

impl HostPattern {
    /// Parses a host pattern from a string token.
    ///
    /// Accepts `*`, `all`, IPv4/IPv6 addresses with optional CIDR prefix,
    /// and hostname patterns (exact, suffix with leading `.`, or wildcard).
    pub(crate) fn parse(token: &str) -> Result<Self, String> {
        let token = token.trim();
        if token.is_empty() {
            return Err("host pattern must be non-empty".to_owned());
        }

        if token == "*" || token.eq_ignore_ascii_case("all") {
            return Ok(Self::Any);
        }

        // upstream: access.c:41-42 - a token of the form `@name` tests the
        // client's resolved hostname for membership in the netgroup `name`
        // (`innetgr(tok + 1, host, NULL, NULL)`). A bare `@` (no name) is not a
        // netgroup (`tok[1]` is required, access.c:41) and falls through to be
        // treated as an ordinary hostname token. The name is lowercased to
        // match upstream's `strlower(list2)` over the whole host list
        // (access.c:251).
        if let Some(name) = token.strip_prefix('@') {
            if !name.is_empty() {
                return Ok(Self::Netgroup(name.to_ascii_lowercase()));
            }
        }

        let (address_str, prefix_text) = if let Some((addr, mask)) = token.split_once('/') {
            (addr, Some(mask))
        } else {
            (token, None)
        };

        if let Ok(ipv4) = address_str.parse::<Ipv4Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv4 prefix length".to_owned())
                })
                .transpose()?;
            return Self::from_ipv4(ipv4, prefix.unwrap_or(32));
        }

        if let Ok(ipv6) = address_str.parse::<Ipv6Addr>() {
            let prefix = prefix_text
                .map(|value| {
                    value
                        .parse::<u8>()
                        .map_err(|_| "invalid IPv6 prefix length".to_owned())
                })
                .transpose()?;
            return Self::from_ipv6(ipv6, prefix.unwrap_or(128));
        }

        if prefix_text.is_some() {
            return Err("invalid host pattern; expected IPv4/IPv6 address".to_owned());
        }

        HostnamePattern::parse(address_str).map(Self::Hostname)
    }

    fn from_ipv4(addr: Ipv4Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 32 {
            return Err("IPv4 prefix length must be between 0 and 32".to_owned());
        }

        if prefix == 0 {
            return Ok(Self::Ipv4 {
                network: Ipv4Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 32 - u32::from(prefix);
        let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
        let network = u32::from(addr) & mask;
        Ok(Self::Ipv4 {
            network: Ipv4Addr::from(network),
            prefix,
        })
    }

    fn from_ipv6(addr: Ipv6Addr, prefix: u8) -> Result<Self, String> {
        if prefix > 128 {
            return Err("IPv6 prefix length must be between 0 and 128".to_owned());
        }

        if prefix == 0 {
            return Ok(Self::Ipv6 {
                network: Ipv6Addr::UNSPECIFIED,
                prefix,
            });
        }

        let shift = 128 - u32::from(prefix);
        let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
        let network = u128::from(addr) & mask;
        Ok(Self::Ipv6 {
            network: Ipv6Addr::from(network),
            prefix,
        })
    }

    /// Returns whether the given IP address and peer hostname match this
    /// pattern.
    ///
    /// `hostname` is never empty - it is either a resolved name or one of
    /// upstream's `UNKNOWN`/`UNDETERMINED` sentinels - which is why the
    /// hostname and netgroup arms match it unconditionally. upstream:
    /// access.c:37-38 refuses only a NULL or empty `host`, a state upstream
    /// never reaches either.
    fn matches(&self, addr: IpAddr, hostname: &str) -> bool {
        match (self, addr) {
            (Self::Any, _) => true,
            (Self::Ipv4 { network, prefix }, IpAddr::V4(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 32 - u32::from(*prefix);
                    let mask = u32::MAX.checked_shl(shift).unwrap_or(0);
                    (u32::from(candidate) & mask) == u32::from(*network)
                }
            }
            (Self::Ipv6 { network, prefix }, IpAddr::V6(candidate)) => {
                if *prefix == 0 {
                    true
                } else {
                    let shift = 128 - u32::from(*prefix);
                    let mask = u128::MAX.checked_shl(shift).unwrap_or(0);
                    (u128::from(candidate) & mask) == u128::from(*network)
                }
            }
            (Self::Hostname(pattern), _) => pattern.matches(hostname),
            // upstream: access.c:41-42 `innetgr(tok + 1, host, NULL, NULL)` -
            // the client's resolved hostname is tested for netgroup membership.
            // Like the reverse-DNS name match, this needs a resolved hostname;
            // without one (access.c:37-38 `if (!host || !*host) return 0`) it
            // never matches. Resolution goes through the `module_state`
            // netgroup seam, a no-op returning false on musl/Windows.
            (Self::Netgroup(name), _) => module_state::netgroup_contains(name, hostname),
            _ => false,
        }
    }

    /// Returns whether this pattern requires a resolved hostname.
    ///
    /// Both hostname-pattern and `@netgroup` tokens are evaluated against the
    /// client's resolved hostname (upstream access.c:37-38, 46), so a deny rule
    /// of either kind must fail closed when no hostname is available
    /// (GHSA-rjfm-3w2m-jf4f).
    const fn requires_hostname(&self) -> bool {
        matches!(self, Self::Hostname(_) | Self::Netgroup(_))
    }

    /// Forward-resolves a config-specified hostname token and matches the
    /// connecting `addr` against the token's A/AAAA records.
    ///
    /// This mirrors the forward-DNS branch of upstream `access.c:match_hostname`
    /// (access.c:49-70): when `forward lookup` is enabled and the token is a
    /// simple hostname (not an address or wildcarded entry), rsync resolves the
    /// token via name lookup and compares the connecting address against the
    /// returned records. It complements the reverse-DNS name-pattern match in
    /// [`HostPattern::matches`] - a peer is admitted/denied by a hostname rule
    /// either because its reverse-DNS name matches the pattern or because the
    /// rule's hostname forward-resolves to the peer's address.
    ///
    /// Resolution is gated on `forward_lookup` (upstream `allow_forward_dns`
    /// from `lp_forward_lookup`, access.c:49) and applies only to the
    /// [`HostPattern::Hostname`] variant; address and CIDR variants are matched
    /// numerically by [`HostPattern::matches`] and never forward-resolved.
    ///
    /// `deny` says which list is being scanned, and it is the whole of
    /// CVE-2026-70452's sibling fix: a token the resolver cannot resolve
    /// returns `deny` (access.c:57-63), so an unresolvable **deny** token
    /// matches - we cannot prove the peer is not the denied host - while an
    /// unresolvable **allow** token still does not. Both gates above return 0
    /// regardless of `deny`, exactly as upstream does at access.c:49-53.
    fn forward_resolve_matches(&self, addr: IpAddr, forward_lookup: bool, deny: bool) -> bool {
        if !forward_lookup {
            return false;
        }

        match self {
            Self::Hostname(pattern) => pattern.forward_resolve_matches(addr, deny),
            _ => false,
        }
    }
}

/// Returns whether a `hosts allow`/`hosts deny` token is a simple hostname
/// eligible for forward-DNS resolution.
///
/// upstream: access.c:52-54 - the forward lookup is skipped when the token is
/// an address (consisting solely of dots and digits) or a wildcarded/netmask
/// entry (containing any of `:` `/` `*` `?` `[`). Only simple hostnames are
/// forward-resolved.
fn token_is_forward_resolvable(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }

    // access.c:53 `!tok[strspn(tok, ".0123456789")]` - a token made up entirely
    // of dots and digits is an address, not a hostname.
    if token.bytes().all(|b| b == b'.' || b.is_ascii_digit()) {
        return false;
    }

    // access.c:53 `tok[strcspn(tok, ":/*?[")]` - address/wildcard
    // metacharacters disqualify the token from forward resolution.
    !token
        .bytes()
        .any(|b| matches!(b, b':' | b'/' | b'*' | b'?' | b'['))
}

/// A pattern for matching hostnames.
///
/// Supports exact matching, suffix matching (leading dot), and wildcard matching.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HostnamePattern {
    kind: HostnamePatternKind,
    /// The original (lowercased) token text, retained for forward-DNS
    /// resolution. Upstream `access.c:match_hostname` forward-resolves the raw
    /// token; retaining it here lets [`HostnamePattern::forward_resolve_matches`]
    /// resolve exactly what upstream would, independent of the reverse-match
    /// pattern kind.
    original: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum HostnamePatternKind {
    Exact(String),
    Suffix(String),
    Wildcard(String),
}

impl HostnamePattern {
    fn parse(pattern: &str) -> Result<Self, String> {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            return Err("host pattern must be non-empty".to_owned());
        }

        // upstream: access.c:251 `strlower(list2)` lowercases the whole host
        // list before tokenizing; the token is used verbatim (dots retained)
        // for the forward `gethostbyname` lookup at access.c:57.
        let original = trimmed.to_ascii_lowercase();

        let normalized = trimmed.trim_end_matches('.');
        let lower = normalized.to_ascii_lowercase();

        if lower.bytes().any(is_wildmatch_metachar) {
            return Ok(Self {
                kind: HostnamePatternKind::Wildcard(lower),
                original,
            });
        }

        if lower.starts_with('.') {
            let suffix = lower.trim_start_matches('.').to_owned();
            return Ok(Self {
                kind: HostnamePatternKind::Suffix(suffix),
                original,
            });
        }

        Ok(Self {
            kind: HostnamePatternKind::Exact(lower),
            original,
        })
    }

    /// Forward-resolves this hostname token and matches `addr` against the
    /// resolved A/AAAA records.
    ///
    /// upstream: access.c:52-70 - forward DNS applies only to simple hostname
    /// tokens; the token is resolved and each returned address is compared to
    /// the connecting address (access.c:60-61). The eligibility gate is
    /// [`token_is_forward_resolvable`]; resolution goes through the shared
    /// [`module_state::forward_resolve`] seam so failures fail closed.
    fn forward_resolve_matches(&self, addr: IpAddr, deny: bool) -> bool {
        if !token_is_forward_resolvable(&self.original) {
            return false;
        }

        match module_state::forward_resolve(&self.original) {
            // upstream: access.c:66-73 - a successful lookup matches only when
            // one of the returned records IS the peer.
            Some(resolved) => resolved.into_iter().any(|record| record == addr),
            // upstream: access.c:57-63 - a token the resolver cannot resolve
            // returns `deny`, so a deny rule fails CLOSED.
            None => deny,
        }
    }

    fn matches(&self, hostname: &str) -> bool {
        // Every pattern kind is lowercased at parse time (mirroring upstream's
        // `strlower(list2)`, access.c:251), so the comparison must fold the HOST
        // too - upstream's matcher is `iwildmatch`, the case-INSENSITIVE form
        // (access.c:46). Comparing directly worked only while every host arrived
        // pre-lowercased by `normalize_hostname_owned`, and failed silently for
        // `UNKNOWN`/`UNDETERMINED`, which upstream documents as usable in a
        // `hosts allow` line (clientname.c:93-95) and which are uppercase.
        let folded = hostname.to_ascii_lowercase();
        let hostname = folded.as_str();
        match &self.kind {
            HostnamePatternKind::Exact(expected) => hostname == expected,
            HostnamePatternKind::Suffix(suffix) => {
                if suffix.is_empty() {
                    return true;
                }

                if hostname == suffix {
                    return true;
                }

                if hostname.len() <= suffix.len() {
                    return false;
                }

                hostname.ends_with(suffix)
                    && hostname
                        .as_bytes()
                        .get(hostname.len() - suffix.len() - 1)
                        .is_some_and(|byte| *byte == b'.')
            }
            // upstream: access.c:46 `iwildmatch(tok, host)` - the token is
            // matched with the full shell-glob matcher, not a `*`/`?`-only
            // one, so bracket expressions work in a host token.
            HostnamePatternKind::Wildcard(pattern) => {
                filters::iwildmatch(pattern.as_bytes(), hostname.as_bytes())
            }
        }
    }
}

/// Returns whether `byte` makes a `hosts allow`/`hosts deny` token a glob
/// rather than a literal name.
///
/// upstream: access.c:53 `tok[strcspn(tok, ":/*?[")]` - `[` is a wildcard
/// metacharacter, on equal footing with `*` and `?`, and disqualifies the token
/// from forward DNS for exactly that reason. `\` joins them because
/// `iwildmatch()` reads it as an escape. `:` and `/` are absent here: they
/// never reach a hostname pattern, having been consumed as address or CIDR
/// syntax by [`HostPattern::parse`].
const fn is_wildmatch_metachar(byte: u8) -> bool {
    matches!(byte, b'*' | b'?' | b'[' | b'\\')
}

/// Warns the operator that `proxy protocol = true` with no trusted-proxy list
/// rejects every connection.
///
/// upstream: clientserver.c:1747-1756
///
/// ```c
/// /* "proxy protocol = true" with no trusted-proxy list rejects every
///  * connection as an untrusted proxy peer (fail-closed).  That is intended,
///  * but silent at startup, so warn the operator while stderr is still open. */
/// if (lp_proxy_protocol()
///  && (!lp_proxy_protocol_hosts() || !*lp_proxy_protocol_hosts())) {
///         rprintf(FWARNING, "\"proxy protocol = true\" but \"proxy protocol hosts\" is unset:" ...);
/// }
/// ```
///
/// The fail-closed default is deliberate, which is exactly why it needs a
/// voice: without this line the operator sees a daemon that accepts a TCP
/// connection and then drops every one of them, with the reason recorded only
/// per-connection.
pub(in crate::daemon) fn warn_if_proxy_protocol_trusts_nobody(
    policy: &ProxyProtocolPolicy,
    log_sink: Option<&SharedLogSink>,
) {
    if !policy.rejects_every_peer() {
        return;
    }

    let text = concat!(
        "\"proxy protocol = true\" but \"proxy protocol hosts\" is unset:",
        " all connections will be rejected as untrusted proxy peers.",
        "  Set \"proxy protocol hosts\" to your trusted proxy's address."
    )
    .to_owned();
    let message = rsync_warning!(text).with_role(Role::Daemon);
    if let Some(log) = log_sink {
        log_message(log, &message);
    } else {
        eprintln!("{message}");
    }
}

/// What the daemon must do with an incoming connection's PROXY protocol
/// header, given the configured policy and the peer's real address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::daemon) enum ProxyHeaderDecision {
    /// `proxy protocol = false`: no header is expected, so read none.
    NotRequired,
    /// The peer is a listed trusted proxy; believe the header it sends.
    Trusted,
    /// A header is required but this peer may not supply one. Upstream logs
    /// the refusal and drops the connection (clientserver.c:1393-1396,
    /// 1443-1446).
    Untrusted,
}

/// The daemon's PROXY protocol configuration as one value.
///
/// upstream keeps `proxy protocol` and `proxy protocol hosts` as two
/// directives (daemon-parm.h) but reads them at one site: `rsync_module()`
/// consults `lp_proxy_protocol()` and, if set, calls `proxy_peer_allowed()`,
/// which consults `lp_proxy_protocol_hosts()` (clientserver.c:1443-1446). The
/// two are only ever meaningful together - a trusted-proxy list with the
/// feature off decides nothing, and the feature on without a list rejects
/// everyone - so oc carries them as one value rather than as a bool and a
/// list that could drift apart while being threaded to the session handler.
#[derive(Clone, Debug, Default)]
pub(in crate::daemon) enum ProxyProtocolPolicy {
    /// `proxy protocol = false` (upstream's default).
    #[default]
    Disabled,
    /// `proxy protocol = true`, carrying the `proxy protocol hosts` list. An
    /// empty list is legal configuration and trusts nobody.
    Enabled(Arc<Vec<HostPattern>>),
}

impl ProxyProtocolPolicy {
    /// Builds the policy from the two parsed directives.
    pub(in crate::daemon) fn new(enabled: bool, trusted: Vec<HostPattern>) -> Self {
        if enabled {
            Self::Enabled(Arc::new(trusted))
        } else {
            Self::Disabled
        }
    }

    /// Decides how to treat a connection arriving from `addr`.
    pub(in crate::daemon) fn decide(&self, addr: IpAddr) -> ProxyHeaderDecision {
        match self {
            Self::Disabled => ProxyHeaderDecision::NotRequired,
            Self::Enabled(trusted) if allow_proxy_protocol_peer(trusted, addr) => {
                ProxyHeaderDecision::Trusted
            }
            Self::Enabled(_) => ProxyHeaderDecision::Untrusted,
        }
    }

    /// Returns whether `proxy protocol = true` was set with no trusted-proxy
    /// list - the fail-closed combination upstream warns about at startup.
    ///
    /// upstream: clientserver.c:1750-1751.
    pub(in crate::daemon) fn rejects_every_peer(&self) -> bool {
        matches!(self, Self::Enabled(trusted) if trusted.is_empty())
    }
}

/// Returns whether `addr` is a trusted proxy allowed to supply a PROXY
/// protocol header.
///
/// upstream: access.c:300-306
///
/// ```c
/// int allow_proxy_protocol_peer(const char *list, const char *addr, const char **host_ptr)
/// {
///         if (!list || !*list)
///                 return 0;
///         allow_forward_dns = 0;
///         return access_match(list, addr, host_ptr, 0);
/// }
/// ```
///
/// Two properties of that body are the whole gate, and both are deliberate:
///
/// 1. **An empty list rejects every peer.** It is not "unset means allow" -
///    `proxy protocol = true` with no trusted-proxy list fail-closes, which is
///    why upstream warns about that combination at startup
///    (clientserver.c:1750-1755).
/// 2. **No DNS is consulted.** `allow_forward_dns = 0` disables the
///    forward-resolve half, and the caller passes the `UNDETERMINED` sentinel
///    rather than a resolved name (clientserver.c:1390-1393), so
///    `match_hostname` can only compare a token against that sentinel. In
///    practice the list is matched numerically - a name-based trusted-proxy
///    token cannot match a real peer. Passing the sentinel here reproduces that
///    exactly, and costs no lookup.
fn allow_proxy_protocol_peer(list: &[HostPattern], addr: IpAddr) -> bool {
    list.iter()
        .any(|pattern| pattern.matches(addr, module_state::UNDETERMINED_HOSTNAME))
}

/// Parses a host allow/deny list from a config directive value.
///
/// Splits the value by commas and whitespace and parses each token as a
/// [`HostPattern`]. An invalid token is an error; an *empty* value is not.
///
/// upstream: access.c:275-278 - `allow_access()` normalises an empty list
/// string to `NULL` (`if (allow_list && !*allow_list) allow_list = NULL;`)
/// before it decides anything, so `hosts allow =` is legal config meaning
/// "no list", indistinguishable from the directive being absent.
/// `allow_proxy_protocol_peer` (access.c:302-303) reads its own list through
/// the same `!list || !*list` guard, where it means "trust nobody".
///
/// The empty list is what carries that meaning here, because both oc
/// consumers already key on emptiness rather than on an `Option`:
/// `ModuleDefinition::permits` skips the allow short-circuit when
/// `hosts_allow` is empty, and the proxy-peer check matches nothing against
/// an empty pattern set. Refusing the value instead aborted the daemon at
/// startup, where upstream serves.
///
/// upstream: params.c:62 - "Leading and trailing whitespace is stripped
/// from" the value before it is stored, so a whitespace-only value is the
/// empty string on both implementations; oc's parser trims identically
/// (`config_parsing/parser.rs`), which is what makes this equivalence exact
/// rather than approximate.
fn parse_host_list(
    value: &str,
    config_path: &Path,
    line: usize,
    directive: &str,
) -> Result<Vec<HostPattern>, DaemonError> {
    let mut patterns = Vec::new();

    for token in value.split(|ch: char| ch.is_ascii_whitespace() || ch == ',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }

        let pattern = HostPattern::parse(token).map_err(|message| {
            config_parse_error(
                config_path,
                line,
                format!("{directive} directive contains invalid pattern '{token}': {message}"),
            )
        })?;
        patterns.push(pattern);
    }

    Ok(patterns)
}
