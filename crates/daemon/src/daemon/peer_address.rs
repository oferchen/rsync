//! Resolution of the connecting peer's address for the two stdio daemon entry
//! points, mirroring upstream's `client_addr()`.
//!
//! upstream: clientname.c:55-84 - `client_addr(int fd)` has two arms, chosen by
//! the sign of `am_daemon`:
//!
//! ```c
//! if (am_daemon < 0) {              /* daemon over --rsh mode */
//!     strlcpy(ipaddr_buf, "0.0.0.0", sizeof ipaddr_buf);
//!     if ((env_str = getenv("REMOTE_HOST"))    != NULL
//!      || (env_str = getenv("SSH_CONNECTION")) != NULL
//!      || (env_str = getenv("SSH_CLIENT"))     != NULL
//!      || (env_str = getenv("SSH2_CLIENT"))    != NULL) {
//!         strlcpy(ipaddr_buf, env_str, sizeof ipaddr_buf);
//!         if ((p = strchr(ipaddr_buf, ' ')) != NULL) *p = '\0';
//!     }
//!     if (valid_ipaddr(ipaddr_buf, True)) return ipaddr_buf;
//! }
//! client_sockaddr(fd, &ss, &length);
//! getnameinfo(..., NI_NUMERICHOST);
//! ```
//!
//! and `client_sockaddr()` (clientname.c:319-327) does NOT invent a fallback
//! when the socket cannot name its peer - it aborts the connection:
//!
//! ```c
//! if (getpeername(fd, (struct sockaddr *) ss, ss_len)) {
//!     rsyserr(FLOG, errno, "getpeername on fd%d failed", fd);
//!     exit_cleanup(RERR_SOCKETIO);
//! }
//! ```
//!
//! The two entry points are therefore NOT the same case: under inetd, fd 0 *is*
//! the connected socket and `getpeername` succeeds, so there is a real address
//! to be had; under a remote shell, fd 0 is a pipe and the address can only come
//! from the environment. A synthetic `127.0.0.1` for both would make every
//! `hosts allow` / `hosts deny` rule evaluate against a fake localhost -
//! admitting every client under the canonical `hosts allow = 127.0.0.1`, and
//! rejecting every legitimate client under an allow-list naming real subnets.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// The connecting peer as the daemon reports and checks it.
///
/// upstream: clientname.c:55-84 `client_addr()` keeps the peer as a string;
/// clientserver.c:775 exports it as `RSYNC_HOST_ADDR`, the log and `@ERROR`
/// lines print it (clientserver.c:778-791), and `allow_access()` parses it
/// again for the `hosts allow`/`hosts deny` match (access.c:84-130). This
/// value carries both facts so no consumer re-derives one from the other: the
/// text that is shown and, when the text names one, the address that is
/// matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PeerAddress {
    text: String,
    ip: Option<IpAddr>,
    scope_id: u32,
}

impl PeerAddress {
    /// A peer named by a socket address, shown as its address, with the IPv6
    /// scope id the socket reported (0 for IPv4).
    pub(crate) fn from_socket(addr: SocketAddr) -> Self {
        let scope_id = match addr {
            SocketAddr::V6(v6) => v6.scope_id(),
            SocketAddr::V4(_) => 0,
        };
        Self {
            text: addr.ip().to_string(),
            ip: Some(addr.ip()),
            scope_id,
        }
    }

    /// A peer named by a remote-shell environment string that `valid_ipaddr`
    /// accepted. The string is kept exactly as given and the address is
    /// whatever this platform's numeric-host reading makes of it.
    ///
    /// upstream: clientname.c:76 returns `ipaddr_buf` itself, so
    /// `RSYNC_HOST_ADDR` and the log lines show `010.1.2.3` or `fe80::1%eth0`
    /// verbatim. A string the reading rejects has no address at all - never a
    /// stand-in such as `0.0.0.0` - and the access check decides it by the
    /// unresolved-peer rule in `config_helpers/host_pattern.rs`.
    pub(in crate::daemon) fn from_remote_shell(text: &str) -> Self {
        let parsed = numeric_host(
            NumericHostRule::HOST,
            text,
            platform::net_interface::interface_index,
        );
        Self {
            text: text.to_owned(),
            ip: parsed.map(|(ip, _)| ip),
            scope_id: parsed.map_or(0, |(_, scope_id)| scope_id),
        }
    }

    /// The address `hosts allow`/`hosts deny` and reverse lookups use, or
    /// `None` when the peer string names no address this platform can read.
    pub(crate) const fn ip(&self) -> Option<IpAddr> {
        self.ip
    }

    /// The peer's IPv6 scope id, 0 when it has none.
    pub(crate) const fn scope_id(&self) -> u32 {
        self.scope_id
    }

    /// The peer as `RSYNC_HOST_ADDR` and the log lines show it.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Display for PeerAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text())
    }
}

/// Peer-address environment variables consulted on the remote-shell path, in
/// upstream's order. The first one that is set wins, even if a later one would
/// parse and it does not.
///
/// upstream: clientname.c:66-69 - the `||` chain assigns `env_str` from the
/// first non-NULL `getenv`, so ordering is significant.
const REMOTE_SHELL_PEER_ENV: [&str; 4] =
    ["REMOTE_HOST", "SSH_CONNECTION", "SSH_CLIENT", "SSH2_CLIENT"];

/// The address upstream seeds before consulting the environment, and the one it
/// keeps when no variable is set.
///
/// This value is load-bearing for access control rather than cosmetic: it
/// matches no realistic `hosts allow` token, so a remote-shell daemon with no
/// peer information in its environment fails CLOSED. `127.0.0.1` failed open
/// against exactly the rule an operator writes to mean "local only".
///
/// upstream: clientname.c:65 - `strlcpy(ipaddr_buf, "0.0.0.0", ...)`.
pub(in crate::daemon) const REMOTE_SHELL_DEFAULT_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);

/// Capacity of upstream's `ipaddr_buf`; `strlcpy` keeps at most one byte less.
///
/// upstream: clientname.c:37 - `static char ipaddr_buf[100]`.
const IPADDR_BUF_LEN: usize = 100;

/// Parses one environment value the way upstream does: copy it into the
/// 100-byte buffer, truncate at the first space, then accept it only if
/// `valid_ipaddr(buf, True)` does.
///
/// `SSH_CONNECTION` is `"<client-ip> <client-port> <server-ip> <server-port>"`
/// and `SSH_CLIENT` is `"<client-ip> <client-port> <server-port>"`, so the
/// leading field is the peer address in both.
///
/// Returns `None` when upstream would reject the value and fall through to the
/// socket. An accepted value becomes the peer string as it stands, with
/// whatever address this platform reads from it - see
/// [`PeerAddress::from_remote_shell`].
///
/// upstream: clientname.c:70-77 plus `valid_ipaddr(buf, True)`.
fn parse_peer_env_value(value: &str) -> Option<PeerAddress> {
    let field = peer_env_field(value);
    if !valid_ipaddr(field, true) {
        return None;
    }
    std::str::from_utf8(field)
        .ok()
        .map(PeerAddress::from_remote_shell)
}

/// The part of an environment value upstream keeps in `ipaddr_buf`: at most 99
/// bytes, cut at the first space.
///
/// upstream: clientname.c:71-74.
fn peer_env_field(value: &str) -> &[u8] {
    let bytes = value.as_bytes();
    let copied = &bytes[..bytes.len().min(IPADDR_BUF_LEN - 1)];
    copied.split(|&b| b == b' ').next().unwrap_or(copied)
}

/// Byte-for-byte port of upstream's address validator, including its quirks:
/// IPv4 octets are read with `strtol`, so leading zeros, a sign and leading
/// whitespace count toward the 3-character limit; IPv6 groups take 1-4 hex
/// digits, `::` may appear once, a trailing dotted quad is allowed only in the
/// last 32 bits, and with `allow_scope` a `%` suffix of alphanumerics follows.
///
/// upstream: clientname.c:469-536 `valid_ipaddr()`.
fn valid_ipaddr(s: &[u8], allow_scope: bool) -> bool {
    let at = |i: usize| s.get(i).copied().unwrap_or(0);
    let hex = |i: usize| at(i).is_ascii_hexdigit();
    let mut i = 0;

    if s.contains(&b':') {
        let mut saw_double_colon = false;
        if at(i) == b':' {
            i += 1;
            if at(i) != b':' {
                return false;
            }
            saw_double_colon = true;
            i += 1;
        }
        let mut ipv4_at_end = false;
        for count in 0..8 {
            if at(i) == 0 {
                return saw_double_colon;
            }
            if allow_scope && at(i) == b'%' {
                if saw_double_colon {
                    break;
                }
                return false;
            }
            let rest = &s[i..];
            if !rest.contains(&b':') && rest.contains(&b'.') {
                if (!saw_double_colon && count != 6) || (saw_double_colon && count > 6) {
                    return false;
                }
                ipv4_at_end = true;
                break;
            }
            if !hex(i) {
                return false;
            }
            i += 1;
            if hex(i) {
                i += 1;
                if hex(i) {
                    i += 1;
                    if hex(i) {
                        i += 1;
                        if hex(i) {
                            return false;
                        }
                    }
                }
            }
            if at(i) == b':' {
                i += 1;
                if at(i) == 0 {
                    return false;
                }
                if at(i) == b':' {
                    if saw_double_colon {
                        return false;
                    }
                    saw_double_colon = true;
                    i += 1;
                }
            }
        }
        if !ipv4_at_end {
            if allow_scope && at(i) == b'%' {
                i += 1;
                while at(i).is_ascii_alphanumeric() {
                    i += 1;
                }
            }
            return at(i) == 0 && i > 0 && s[i - 1] != b'%';
        }
    }

    for octet in 0..4 {
        if octet > 0 {
            if at(i) != b'.' {
                return false;
            }
            i += 1;
        }
        let Some((value, end)) = strtol_decimal(s, i) else {
            return false;
        };
        if !(0..=255).contains(&value) || end > i + 3 {
            return false;
        }
        i = end;
    }
    at(i) == 0
}

/// `strtol(s + start, &end, 10)` restricted to what `valid_ipaddr` observes:
/// skips C `isspace` bytes, takes an optional sign, then decimal digits.
/// Returns `None` when no digit was consumed (`end <= s` upstream).
fn strtol_decimal(s: &[u8], start: usize) -> Option<(i64, usize)> {
    let mut i = start;
    while matches!(s.get(i), Some(b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')) {
        i += 1;
    }
    let negative = match s.get(i) {
        Some(b'-') => {
            i += 1;
            true
        }
        Some(b'+') => {
            i += 1;
            false
        }
        _ => false,
    };
    let digits_start = i;
    let mut value: i64 = 0;
    while let Some(digit) = s.get(i).filter(|b| b.is_ascii_digit()) {
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(digit - b'0'));
        i += 1;
    }
    (i > digits_start).then_some((if negative { -value } else { value }, i))
}

/// How this platform's `getaddrinfo(addr, NULL, AI_NUMERICHOST)` reads a peer
/// string that `valid_ipaddr()` accepted.
///
/// upstream: access.c:92 and :219 parse the peer string with that call before
/// any `hosts allow`/`hosts deny` address comparison, so the reading belongs to
/// the C library, not to rsync, and differs by platform. oc reproduces each
/// reading in Rust instead of calling the resolver: the result is fixed per
/// target, needs no DNS, and every rule is testable on every host.
///
/// `valid_ipaddr()` has already limited IPv4 to four parts of at most three
/// digits, so the `inet_aton` forms it rejects (hex `0x8.1.2.3`, short forms
/// `8.66051` and `134283779`) never reach a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NumericHostRule {
    /// glibc and musl. IPv4 parts are read by `inet_aton`, so a leading `0`
    /// makes a part octal (`010` is 8, `08` fails). An interface-name scope
    /// resolves only on a link-local address, and an unknown name fails.
    /// Sources: glibc resolv/inet_addr.c `inet_aton_end` and
    /// inet/inet6_scopeid_pton.c; musl src/network/inet_aton.c and
    /// src/network/lookup_ipliteral.c. The tables were also measured against
    /// both libraries' `getaddrinfo`.
    InetAton,
    /// Darwin, measured on Darwin 27 via `getaddrinfo` (not read from source).
    /// IPv4 parts are decimal with leading zeros allowed (`010` is 10, `08` is
    /// 8). An interface-name scope resolves on any address, and an unknown
    /// name gives scope 0.
    LenientDecimal,
    /// Every other target, Windows included, none of them measured. Plain
    /// decimal without leading zeros, and an interface-name scope must
    /// resolve. Anything else has no address, which the access check fails
    /// closed on.
    StrictDecimal,
}

impl NumericHostRule {
    #[cfg(target_os = "linux")]
    const HOST: Self = Self::InetAton;
    #[cfg(target_vendor = "apple")]
    const HOST: Self = Self::LenientDecimal;
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    const HOST: Self = Self::StrictDecimal;

    /// Reads a four-part dotted quad.
    fn ipv4(self, text: &str) -> Option<Ipv4Addr> {
        if self == Self::StrictDecimal {
            return text.parse().ok();
        }
        let mut octets = [0u8; 4];
        let mut parts = text.split('.');
        for octet in &mut octets {
            let part = parts.next()?;
            if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let octal = self == Self::InetAton && part.len() > 1 && part.starts_with('0');
            *octet = u8::from_str_radix(part, if octal { 8 } else { 10 }).ok()?;
        }
        parts.next().is_none().then(|| Ipv4Addr::from(octets))
    }

    /// Reads an IPv6 address without its scope. glibc's and musl's
    /// `inet_pton` take an embedded dotted quad as plain decimal only; Darwin
    /// reads it as it reads IPv4 (`::ffff:010.1.2.3` is `::ffff:10.1.2.3`).
    fn ipv6(self, text: &str) -> Option<Ipv6Addr> {
        if self == Self::LenientDecimal
            && let Some((head, quad)) = text.rsplit_once(':')
            && quad.contains('.')
        {
            let quad = self.ipv4(quad)?;
            return format!("{head}:{quad}").parse().ok();
        }
        text.parse().ok()
    }

    /// The scope id for an interface-name scope on `addr`, given what
    /// `if_nametoindex` returned for the name.
    fn named_scope(self, addr: Ipv6Addr, index: Option<u32>) -> Option<u32> {
        match self {
            Self::InetAton => index.filter(|_| names_an_interface_scope(addr)),
            Self::LenientDecimal => Some(index.unwrap_or(0)),
            Self::StrictDecimal => index,
        }
    }
}

/// Reads `text` by `rule`, returning the address and its IPv6 scope id, or
/// `None` where the platform's `getaddrinfo(AI_NUMERICHOST)` fails.
///
/// A `%` scope that is all digits is the index itself (`%01` is 1, at most
/// `u32::MAX`) on every platform and any address; a name goes through
/// `interface_index` and then the rule.
fn numeric_host(
    rule: NumericHostRule,
    text: &str,
    interface_index: impl FnOnce(&str) -> Option<u32>,
) -> Option<(IpAddr, u32)> {
    let Some((addr, scope)) = text.split_once('%') else {
        return if text.contains(':') {
            rule.ipv6(text).map(|v6| (IpAddr::V6(v6), 0))
        } else {
            rule.ipv4(text).map(|v4| (IpAddr::V4(v4), 0))
        };
    };
    let v6 = rule.ipv6(addr)?;
    let scope_id = if !scope.is_empty() && scope.bytes().all(|b| b.is_ascii_digit()) {
        scope.parse().ok()?
    } else {
        rule.named_scope(v6, interface_index(scope))?
    };
    Some((IpAddr::V6(v6), scope_id))
}

/// Whether glibc or musl resolves an interface-name scope on `addr`.
///
/// glibc: inet/inet6_scopeid_pton.c - link-local unicast, node-local and
/// link-local multicast. musl: src/network/lookup_ipliteral.c - link-local
/// unicast and link-local multicast only.
fn names_an_interface_scope(addr: Ipv6Addr) -> bool {
    let first = addr.segments()[0];
    let multicast_scope = (first & 0xff00 == 0xff00).then_some(first & 0x000f);
    let node_local = cfg!(not(target_env = "musl")) && multicast_scope == Some(1);
    addr.is_unicast_link_local() || multicast_scope == Some(2) || node_local
}

/// Outcome of resolving a remote-shell peer address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::daemon) enum RemoteShellPeer {
    /// The peer to use: either the string the environment named, or the
    /// `0.0.0.0` default when the environment said nothing.
    Addr(PeerAddress),
    /// An environment variable was set but did not hold a usable address.
    ///
    /// Upstream falls through to `client_sockaddr()` here, which on a remote
    /// shell's pipe fails `getpeername` and aborts the connection. Reporting it
    /// separately lets the caller mirror that abort instead of quietly
    /// substituting a default the operator did not configure. The field is the
    /// text upstream left in `ipaddr_buf` (copied and cut at the first space).
    Unusable(String),
}

/// Resolves the peer address for a daemon started behind a remote shell.
///
/// upstream: clientname.c:63-78 (`am_daemon < 0`).
pub(in crate::daemon) fn remote_shell_peer_addr() -> RemoteShellPeer {
    let Some(raw) = REMOTE_SHELL_PEER_ENV
        .iter()
        .find_map(|name| std::env::var(name).ok())
    else {
        return RemoteShellPeer::Addr(PeerAddress::from_socket(SocketAddr::new(
            REMOTE_SHELL_DEFAULT_ADDR,
            0,
        )));
    };

    parse_peer_env_value(&raw).map_or_else(
        || RemoteShellPeer::Unusable(String::from_utf8_lossy(peer_env_field(&raw)).into_owned()),
        RemoteShellPeer::Addr,
    )
}

/// Normalises an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) to plain IPv4.
///
/// Without this a dual-stack listener reports `::ffff:192.0.2.1` for an IPv4
/// client, which matches neither an IPv4 `hosts allow` pattern nor a reverse
/// lookup, so the normalisation is part of the ACL contract rather than
/// cosmetic.
///
/// upstream: clientname.c:329-356 - `client_sockaddr()` rewrites a V4MAPPED
/// `sockaddr_in6` into a `sockaddr_in` before any lookup or comparison.
///
/// Gated to Unix because its only caller is [`stdin_socket_peer`], which is
/// itself Unix-only: a V4MAPPED peer can only arrive through the stdin socket
/// this module reads. Without the gate the function is dead
/// on Windows, and `-D warnings` (set in `_interop-windows.yml` and
/// `_test-features.yml`) turns that into a build failure.
#[cfg(unix)]
fn normalize_v4_mapped(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(addr, IpAddr::V4),
        IpAddr::V4(_) => addr,
    }
}

/// Reads the peer of fd 0 for a remote-shell daemon whose environment named no
/// usable address.
///
/// upstream: clientname.c:80-81 - `client_sockaddr(fd)` then
/// `getnameinfo(NI_NUMERICHOST)` into `ipaddr_buf`, which still holds the
/// environment field. An IP peer becomes its numeric address (an IPv4-mapped
/// IPv6 one is rewritten to IPv4 first, clientname.c:329-356). A peer that is
/// not an IP socket - the `AF_UNIX` socketpair a local `-e` command hands its
/// child - is rendered by the C library's own `getnameinfo(NI_NUMERICHOST)`
/// through `platform::peer_name::peer_numeric_host`: glibc writes
/// `localhost`, and a library that refuses the family leaves the buffer
/// holding the environment field (clientname.c:71-74, :81).
///
/// `socket2::SockRef::from(&io::Stdin)` borrows the descriptor through `AsFd`
/// without taking ownership, so nothing closes stdin and no `unsafe` is needed.
///
/// # Errors
///
/// Returns the `getpeername` error unchanged when stdin is not a socket (a
/// pipe, as sshd hands a command). The caller must abort the session, as
/// upstream does with `RERR_SOCKETIO` (clientname.c:323-327).
#[cfg(unix)]
pub(in crate::daemon) fn stdin_socket_peer(env_field: &str) -> std::io::Result<PeerAddress> {
    use std::os::fd::AsFd;

    let stdin = std::io::stdin();
    let peer = socket2::SockRef::from(&stdin).peer_addr()?;
    Ok(match peer.as_socket() {
        Some(addr) => {
            PeerAddress::from_socket(SocketAddr::new(normalize_v4_mapped(addr.ip()), addr.port()))
        }
        None => PeerAddress::from_remote_shell(
            platform::peer_name::peer_numeric_host(stdin.as_fd())?
                .as_deref()
                .unwrap_or(env_field),
        ),
    })
}

/// Non-Unix stub: daemon mode is refused on these platforms, so a remote-shell
/// daemon never reaches this.
#[cfg(not(unix))]
pub(in crate::daemon) fn stdin_socket_peer(_env_field: &str) -> std::io::Result<PeerAddress> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "stdin sockets are not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvGuard;
    use std::ffi::OsStr;

    /// The address an environment value yields: `None` when `valid_ipaddr`
    /// rejects it, `Some(None)` when it is kept but names no address.
    fn ip_of(value: &str) -> Option<Option<IpAddr>> {
        parse_peer_env_value(value).map(|peer| peer.ip())
    }

    fn text_of(value: &str) -> Option<String> {
        parse_peer_env_value(value).map(|peer| peer.text().to_owned())
    }

    /// upstream: clientname.c:73-74 - `SSH_CONNECTION` carries four
    /// space-separated fields and only the first is the peer.
    #[test]
    fn env_value_truncates_at_the_first_space() {
        assert_eq!(
            ip_of("192.0.2.7 54321 198.51.100.1 22"),
            Some(Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7))))
        );
        assert_eq!(
            text_of("192.0.2.7 54321 198.51.100.1 22").as_deref(),
            Some("192.0.2.7")
        );
    }

    #[test]
    fn env_value_accepts_a_bare_address() {
        assert_eq!(
            ip_of("198.51.100.9"),
            Some(Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))))
        );
    }

    /// upstream passes `allow_scope = True` to `valid_ipaddr`, so a link-local
    /// address carrying an interface scope is usable, and the scope stays in
    /// the peer string `RSYNC_HOST_ADDR` and the logs show (clientname.c:76).
    #[test]
    fn env_value_accepts_a_scoped_ipv6_address() {
        assert_eq!(
            text_of("fe80::1%eth0 22 fe80::2 22").as_deref(),
            Some("fe80::1%eth0")
        );
        assert_eq!(
            ip_of("fe80::1%1 22 fe80::2 22"),
            Some(Some(IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap())))
        );
    }

    #[test]
    fn env_value_rejects_a_non_address() {
        assert_eq!(ip_of("not-an-address 22"), None);
        assert_eq!(ip_of(""), None);
    }

    /// upstream: `valid_ipaddr()` has no scope branch on the IPv4 path, so an
    /// IPv4 value with a `%` suffix is invalid and upstream falls through to
    /// the socket rather than trusting the variable.
    #[test]
    fn env_value_rejects_a_scope_on_an_ipv4_address() {
        assert_eq!(ip_of("192.0.2.7%eth0 22"), None);
    }

    /// upstream: clientname.c:521-522 - the scope must be non-empty and
    /// alphanumeric (`!*s && s[-1] != '%'`).
    #[test]
    fn env_value_rejects_an_empty_or_non_alphanumeric_scope() {
        assert_eq!(ip_of("fe80::1%"), None);
        assert_eq!(ip_of("fe80::1%eth-0"), None);
    }

    /// upstream: clientname.c:476-483 - a scope is legal mid-address only after
    /// a `::` was seen; a full 8-group address may still carry one at the end.
    #[test]
    fn env_value_scope_placement_follows_upstream() {
        assert_eq!(ip_of("1:2:3:4:5:6:7%eth0"), None);
        assert_eq!(
            ip_of("1:2:3:4:5:6:7:8%1"),
            Some(Some(IpAddr::V6(
                "1:2:3:4:5:6:7:8".parse::<Ipv6Addr>().unwrap()
            )))
        );
    }

    /// upstream reads IPv4 octets with `strtol`, so `010` and `+2` pass
    /// `valid_ipaddr` and the daemon keeps the value as the peer string instead
    /// of consulting the socket (clientname.c:76).
    #[test]
    fn env_value_keeps_strtol_octets_as_the_peer_string() {
        assert_eq!(text_of("010.1.2.3 22").as_deref(), Some("010.1.2.3"));
        assert_eq!(text_of("1.+2.3.4").as_deref(), Some("1.+2.3.4"));
        assert_eq!(ip_of("1.2.3.1000"), None);
        assert_eq!(ip_of("1.2.3.256"), None);
        assert_eq!(ip_of("1.2.3"), None);
        assert_eq!(ip_of("1.2.3.4.5"), None);
    }

    /// The `inet_aton` forms `valid_ipaddr` rejects - hex, and the one-, two-
    /// and three-part short forms - fall through to the socket as upstream
    /// does, whatever the C library would have made of them: glibc reads all
    /// three below as 8.1.2.3.
    #[test]
    fn env_value_rejects_hex_and_short_inet_aton_forms() {
        assert_eq!(ip_of("0x8.1.2.3"), None);
        assert_eq!(ip_of("8.66051"), None);
        assert_eq!(ip_of("134283779"), None);
    }

    /// Task 754: an accepted string this platform cannot read keeps NO address.
    /// oc used to substitute `0.0.0.0`, which a deny rule for the peer's real
    /// address never matched. The address is `None` - never `0.0.0.0` or any
    /// other stand-in - and the text is the string as given.
    #[test]
    fn an_unreadable_accepted_peer_has_no_substitute_address() {
        let mut unreadable = vec!["1.+2.3.4", "-0.1.2.3"];
        if NumericHostRule::HOST != NumericHostRule::LenientDecimal {
            unreadable.extend(["fe80::1%1x", "fe80::1%ocnosuchif0"]);
        }
        if NumericHostRule::HOST == NumericHostRule::InetAton {
            unreadable.extend(["08.1.2.3", "009.1.2.3", "::ffff:010.1.2.3"]);
        }
        for value in unreadable {
            let peer = parse_peer_env_value(value)
                .unwrap_or_else(|| panic!("valid_ipaddr accepts {value}"));
            assert_eq!(peer.ip(), None, "{value} must keep no address");
            assert_eq!(peer.text(), value, "{value} must be kept verbatim");
        }
    }

    /// Only a string that IS a zero address may read as one: `0.0.0.0` is
    /// upstream's seed, not a fallback for strings the platform cannot read.
    #[test]
    fn only_a_zero_address_string_reads_as_the_unspecified_address() {
        let zero = Some(Some(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
        assert_eq!(ip_of("0.0.0.0"), zero);
        for value in [
            "1.+2.3.4",
            "-0.1.2.3",
            "08.1.2.3",
            "010.1.2.3",
            "fe80::1%1x",
        ] {
            assert_ne!(ip_of(value), zero, "{value} must not read as 0.0.0.0");
        }
    }

    /// upstream: clientname.c:71 - `strlcpy` into the 100-byte buffer drops
    /// everything past byte 99, so an over-long scope is cut back to valid.
    #[test]
    fn env_value_is_truncated_to_upstreams_buffer() {
        let mut value = String::from("fe80::1%");
        value.push_str(&"a".repeat(IPADDR_BUF_LEN - 1 - value.len()));
        value.push('-');
        let kept = &value[..IPADDR_BUF_LEN - 1];
        assert_eq!(text_of(&value).as_deref(), Some(kept));
    }

    /// upstream: clientname.c:478-485 - a dotted quad may only fill the last
    /// 32 bits of an IPv6 address.
    #[test]
    fn env_value_ipv6_with_embedded_ipv4_follows_upstream() {
        assert_eq!(
            ip_of("::ffff:192.0.2.1"),
            Some(Some(IpAddr::V6(
                "::ffff:192.0.2.1".parse::<Ipv6Addr>().unwrap()
            )))
        );
        assert_eq!(ip_of("1:2:3:192.0.2.1"), None);
    }

    /// `SSH_CONNECTION`, as `sshd` sets it, names the peer. The full
    /// environment chain is read, so this also proves the earlier variables in
    /// upstream's order are absent rather than shadowing it.
    ///
    /// upstream: clientname.c:66-74.
    #[test]
    fn ssh_connection_names_the_remote_shell_peer() {
        let _env_lock = crate::test_env::ENV_LOCK.lock().expect("lock environment");
        let _remote_host = EnvGuard::remove("REMOTE_HOST");
        let _ssh_client = EnvGuard::remove("SSH_CLIENT");
        let _ssh2_client = EnvGuard::remove("SSH2_CLIENT");
        let _ssh_connection =
            EnvGuard::set("SSH_CONNECTION", OsStr::new("10.1.2.3 5555 10.0.0.1 22"));
        let RemoteShellPeer::Addr(peer) = remote_shell_peer_addr() else {
            panic!("SSH_CONNECTION names a usable peer");
        };
        assert_eq!(peer.ip(), Some(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))));
        assert_eq!(peer.text(), "10.1.2.3");
    }

    /// upstream: clientname.c:65 - with no variable set the seed survives.
    #[test]
    fn no_peer_environment_yields_the_unspecified_seed() {
        let _env_lock = crate::test_env::ENV_LOCK.lock().expect("lock environment");
        let _guards = REMOTE_SHELL_PEER_ENV.map(EnvGuard::remove);
        let RemoteShellPeer::Addr(peer) = remote_shell_peer_addr() else {
            panic!("the seed is a usable peer");
        };
        assert_eq!(peer.ip(), Some(REMOTE_SHELL_DEFAULT_ADDR));
        assert_eq!(peer.text(), "0.0.0.0");
    }

    /// upstream: clientname.c:66 - the FIRST set variable wins even when it is
    /// invalid; a later valid one is never consulted.
    #[test]
    fn an_invalid_first_variable_is_unusable_even_if_a_later_one_parses() {
        let _env_lock = crate::test_env::ENV_LOCK.lock().expect("lock environment");
        let _remote_host = EnvGuard::set("REMOTE_HOST", OsStr::new("not-an-address"));
        let _ssh_connection =
            EnvGuard::set("SSH_CONNECTION", OsStr::new("10.1.2.3 5555 10.0.0.1 22"));
        assert_eq!(
            remote_shell_peer_addr(),
            RemoteShellPeer::Unusable("not-an-address".to_owned())
        );
    }

    /// The security-relevant default. `0.0.0.0` matches no realistic
    /// `hosts allow` token, so an unconfigured remote-shell daemon denies
    /// rather than admits.
    #[test]
    fn default_remote_shell_address_is_unspecified_not_loopback() {
        assert_eq!(REMOTE_SHELL_DEFAULT_ADDR, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_ne!(REMOTE_SHELL_DEFAULT_ADDR, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    /// upstream: clientname.c:329-356 - a V4MAPPED peer is rewritten to IPv4
    /// before any ACL comparison, so `hosts allow = 192.0.2.0/24` matches an
    /// IPv4 client arriving on a dual-stack listener.
    /// Unix-only for the same reason the function is: it has no Windows caller.
    #[cfg(unix)]
    #[test]
    fn v4_mapped_ipv6_normalises_to_ipv4() {
        let mapped: IpAddr = "::ffff:192.0.2.128".parse().unwrap();
        assert_eq!(
            normalize_v4_mapped(mapped),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 128))
        );
    }

    #[cfg(unix)]
    #[test]
    fn genuine_ipv6_is_left_alone() {
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(normalize_v4_mapped(v6), v6);
    }

    /// An `AF_UNIX` peer has no IP address. Upstream serves such a connection
    /// (its `getnameinfo` failure is unchecked, leaving the address empty), so
    /// refusing it would break systemd socket activation with `Accept=yes` and
    /// `RSYNC_CONNECT_PROG`. The sentinel matches no realistic allow token, so
    /// the ACL still fails closed.
    #[test]
    fn a_non_ip_peer_degrades_to_the_unknown_sentinel_not_loopback() {
        let sentinel = SocketAddr::new(REMOTE_SHELL_DEFAULT_ADDR, 0);
        assert_eq!(sentinel.ip(), IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_ne!(sentinel.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}

#[cfg(test)]
mod numeric_host_tests {
    use super::*;

    /// Reads `text` by `rule` with one known interface, `eth0` at index 2, so
    /// the tables do not depend on the host's interfaces.
    fn read(rule: NumericHostRule, text: &str) -> Option<String> {
        numeric_host(rule, text, |name| (name == "eth0").then_some(2))
            .map(|(ip, scope)| format!("{ip}%{scope}"))
    }

    fn check(rule: NumericHostRule, rows: &[(&str, Option<&str>)]) {
        for &(input, expected) in rows {
            assert_eq!(
                read(rule, input).as_deref(),
                expected,
                "{rule:?} reading {input:?}"
            );
        }
    }

    /// glibc 2.44 and musl, measured with `getaddrinfo(AI_NUMERICHOST)` and
    /// matching glibc resolv/inet_addr.c and inet/inet6_scopeid_pton.c and
    /// musl src/network/inet_aton.c and lookup_ipliteral.c. The leading-zero
    /// rows are the security-relevant ones: `hosts deny = 8.1.2.3` must see
    /// 8.1.2.3 in `010.1.2.3`, as rsync 3.5.2 on Linux does.
    #[test]
    fn inet_aton_rule_reads_like_glibc_and_musl() {
        check(
            NumericHostRule::InetAton,
            &[
                ("010.1.2.3", Some("8.1.2.3%0")),
                ("077.1.2.3", Some("63.1.2.3%0")),
                ("00.0.0.0", Some("0.0.0.0%0")),
                ("10.1.2.3", Some("10.1.2.3%0")),
                ("08.1.2.3", None),
                ("009.1.2.3", None),
                ("256.1.2.3", None),
                ("+1.2.3.4", None),
                ("-0.1.2.3", None),
                ("1.+2.3.4", None),
                ("\t1.2.3.4", None),
                // Never reaches a rule (valid_ipaddr rejects it); glibc's own
                // inet_aton would read each as 8.1.2.3.
                ("0x8.1.2.3", None),
                ("8.66051", None),
                ("134283779", None),
                ("::ffff:10.1.2.3", Some("::ffff:10.1.2.3%0")),
                ("::ffff:010.1.2.3", None),
                ("::ffff:+1.2.3.4", None),
                ("fe80::1%99", Some("fe80::1%99")),
                ("fe80::1%01", Some("fe80::1%1")),
                ("fe80::1%0", Some("fe80::1%0")),
                ("2001:db8::1%1", Some("2001:db8::1%1")),
                ("fe80::1%4294967295", Some("fe80::1%4294967295")),
                ("fe80::1%4294967296", None),
                ("fe80::1%eth0", Some("fe80::1%2")),
                ("ff02::1%eth0", Some("ff02::1%2")),
                ("fe80::1%nosuch", None),
                ("fe80::1%1x", None),
                ("2001:db8::1%eth0", None),
                ("::1%eth0", None),
                ("ff05::1%eth0", None),
            ],
        );
    }

    /// glibc names a node-local multicast scope; musl does not.
    #[test]
    fn inet_aton_rule_node_local_scope_follows_the_c_library() {
        let expected = if cfg!(target_env = "musl") {
            None
        } else {
            Some("ff01::1%2")
        };
        assert_eq!(
            read(NumericHostRule::InetAton, "ff01::1%eth0").as_deref(),
            expected
        );
    }

    /// Measured on Darwin 27 via `getaddrinfo(AI_NUMERICHOST)`; not read from
    /// source.
    #[test]
    fn lenient_decimal_rule_reads_like_darwin() {
        check(
            NumericHostRule::LenientDecimal,
            &[
                ("010.1.2.3", Some("10.1.2.3%0")),
                ("08.1.2.3", Some("8.1.2.3%0")),
                ("009.1.2.3", Some("9.1.2.3%0")),
                ("077.1.2.3", Some("77.1.2.3%0")),
                ("+1.2.3.4", None),
                ("-0.1.2.3", None),
                ("\t1.2.3.4", None),
                ("::ffff:010.1.2.3", Some("::ffff:10.1.2.3%0")),
                ("fe80::1%99", Some("fe80::1%99")),
                ("fe80::1%01", Some("fe80::1%1")),
                ("fe80::1%eth0", Some("fe80::1%2")),
                ("2001:db8::1%eth0", Some("2001:db8::1%2")),
                ("fe80::1%nosuch", Some("fe80::1%0")),
            ],
        );
    }

    /// Unmeasured targets: anything but plain decimal and a resolvable or
    /// numeric scope has no address, so the access check fails closed on it.
    #[test]
    fn strict_decimal_rule_rejects_every_ambiguous_form() {
        check(
            NumericHostRule::StrictDecimal,
            &[
                ("10.1.2.3", Some("10.1.2.3%0")),
                ("010.1.2.3", None),
                ("08.1.2.3", None),
                ("+1.2.3.4", None),
                ("::ffff:10.1.2.3", Some("::ffff:10.1.2.3%0")),
                ("::ffff:010.1.2.3", None),
                ("fe80::1%7", Some("fe80::1%7")),
                ("fe80::1%eth0", Some("fe80::1%2")),
                ("fe80::1%nosuch", None),
            ],
        );
    }

    /// The build host's loopback name resolves through the platform wrapper.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_host_rule_resolves_a_real_interface_name() {
        let lo = platform::net_interface::interface_index("lo").expect("loopback index");
        assert_eq!(
            numeric_host(
                NumericHostRule::HOST,
                "fe80::1%lo",
                platform::net_interface::interface_index
            ),
            Some((IpAddr::V6("fe80::1".parse().unwrap()), lo))
        );
    }
}
