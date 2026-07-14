/// A pattern for matching hosts in allow/deny lists.
///
/// Supports wildcards (*), CIDR notation for IP addresses, and hostname patterns.
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

    /// Returns whether the given IP address and optional hostname match this pattern.
    fn matches(&self, addr: IpAddr, hostname: Option<&str>) -> bool {
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
            (Self::Hostname(pattern), _) => {
                hostname.is_some_and(|name| pattern.matches(name))
            }
            _ => false,
        }
    }

    /// Returns whether this pattern requires a resolved hostname.
    const fn requires_hostname(&self) -> bool {
        matches!(self, Self::Hostname(_))
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
    /// numerically by [`HostPattern::matches`] and never forward-resolved. A
    /// lookup that returns no records yields no match (fail-closed), mirroring
    /// the NULL `gethostbyname` return at access.c:57-58.
    fn forward_resolve_matches(&self, addr: IpAddr, forward_lookup: bool) -> bool {
        if !forward_lookup {
            return false;
        }

        match self {
            Self::Hostname(pattern) => pattern.forward_resolve_matches(addr),
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

        if lower.contains('*') || lower.contains('?') {
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
    fn forward_resolve_matches(&self, addr: IpAddr) -> bool {
        if !token_is_forward_resolvable(&self.original) {
            return false;
        }

        module_state::forward_resolve(&self.original)
            .into_iter()
            .any(|resolved| resolved == addr)
    }

    fn matches(&self, hostname: &str) -> bool {
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
            HostnamePatternKind::Wildcard(pattern) => wildcard_match(pattern, hostname),
        }
    }
}

/// Matches a string against a pattern containing `*` and `?` wildcards.
///
/// `*` matches zero or more characters, `?` matches exactly one character.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pattern_bytes = pattern.as_bytes();
    let text_bytes = text.as_bytes();

    let mut pat_index = 0usize;
    let mut text_index = 0usize;
    let mut star_index: Option<usize> = None;
    let mut match_index = 0usize;

    while text_index < text_bytes.len() {
        if pat_index < pattern_bytes.len()
            && (pattern_bytes[pat_index] == b'?'
                || pattern_bytes[pat_index] == text_bytes[text_index])
        {
            pat_index += 1;
            text_index += 1;
        } else if pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
            star_index = Some(pat_index);
            pat_index += 1;
            match_index = text_index;
        } else if let Some(star_pos) = star_index {
            pat_index = star_pos + 1;
            match_index += 1;
            text_index = match_index;
        } else {
            return false;
        }
    }

    while pat_index < pattern_bytes.len() && pattern_bytes[pat_index] == b'*' {
        pat_index += 1;
    }

    pat_index == pattern_bytes.len()
}

/// Parses a host allow/deny list from a config directive value.
///
/// Splits the value by commas and whitespace, parses each token as a
/// `HostPattern`, and returns the list. Returns an error if any token
/// is invalid or the list is empty.
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

    if patterns.is_empty() {
        return Err(config_parse_error(
            config_path,
            line,
            format!("{directive} directive must specify at least one pattern"),
        ));
    }

    Ok(patterns)
}
