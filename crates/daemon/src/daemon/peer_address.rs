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

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

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
/// socket. An accepted value that `IpAddr` cannot represent (a leading-zero or
/// signed IPv4 octet, which `strtol` reads but `getaddrinfo` may not) becomes
/// [`REMOTE_SHELL_DEFAULT_ADDR`]: upstream keeps the raw string, and a string
/// that does not parse as a numeric host matches no `hosts allow` address
/// token. A scope suffix (`fe80::1%eth0`) is dropped before conversion - it
/// names a local interface, not the peer.
///
/// upstream: clientname.c:70-77 plus `valid_ipaddr(buf, True)`.
fn parse_peer_env_value(value: &str) -> Option<IpAddr> {
    let bytes = value.as_bytes();
    let copied = &bytes[..bytes.len().min(IPADDR_BUF_LEN - 1)];
    let field = copied.split(|&b| b == b' ').next().unwrap_or(copied);
    if !valid_ipaddr(field, true) {
        return None;
    }
    let text = std::str::from_utf8(field).ok()?;
    let bare = text.split_once('%').map_or(text, |(addr, _scope)| addr);
    Some(bare.parse().unwrap_or(REMOTE_SHELL_DEFAULT_ADDR))
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

/// Outcome of resolving a remote-shell peer address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::daemon) enum RemoteShellPeer {
    /// An address to use: either one parsed from the environment, or the
    /// `0.0.0.0` default when the environment said nothing.
    Addr(IpAddr),
    /// An environment variable was set but did not hold a usable address.
    ///
    /// Upstream falls through to `client_sockaddr()` here, which on a remote
    /// shell's pipe fails `getpeername` and aborts the connection. Reporting it
    /// separately lets the caller mirror that abort instead of quietly
    /// substituting a default the operator did not configure.
    Unusable,
}

/// Resolves the peer address for a daemon started behind a remote shell.
///
/// upstream: clientname.c:63-78 (`am_daemon < 0`).
pub(in crate::daemon) fn remote_shell_peer_addr() -> RemoteShellPeer {
    let Some(raw) = REMOTE_SHELL_PEER_ENV
        .iter()
        .find_map(|name| std::env::var(name).ok())
    else {
        return RemoteShellPeer::Addr(REMOTE_SHELL_DEFAULT_ADDR);
    };

    parse_peer_env_value(&raw).map_or(RemoteShellPeer::Unusable, RemoteShellPeer::Addr)
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
/// Gated to Unix because its only caller is [`inherited_socket_peer_addr`],
/// which is itself Unix-only: a V4MAPPED peer can only arrive through the
/// inetd stdin socket this module reads. Without the gate the function is dead
/// on Windows, and `-D warnings` (set in `_interop-windows.yml` and
/// `_test-features.yml`) turns that into a build failure.
#[cfg(unix)]
fn normalize_v4_mapped(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(addr, IpAddr::V4),
        IpAddr::V4(_) => addr,
    }
}

/// Reads the real peer address from the socket inherited on stdin.
///
/// Used by the inetd / socket-activation entry point, where fd 0 is the
/// connected socket and the address is genuinely available.
///
/// `socket2::SockRef::from(&io::Stdin)` borrows the descriptor through `AsFd`
/// without taking ownership, so nothing closes stdin and no `unsafe` is needed
/// - the same construction `is_stdin_socket()` already uses to classify fd 0.
///
/// A peer that HAS an address but not an IP one (an `AF_UNIX` socket) degrades
/// to [`REMOTE_SHELL_DEFAULT_ADDR`] rather than failing. `is_stdin_socket()`
/// admits only `AF_INET`/`AF_INET6` peers, as upstream's `is_inetd_socket()`
/// does (clientserver.c:1727-1743), so this arm is a defensive fallback. Were
/// it reached, upstream's `getnameinfo` would fail unchecked (clientname.c:81)
/// and leave `ipaddr_buf` empty; both "" and `0.0.0.0` match no realistic
/// `hosts allow` token, so the ACL outcome is the same.
///
/// # Errors
///
/// Returns the `getpeername` error unchanged when the call itself fails. The
/// caller must abort the session rather than substitute a placeholder; upstream
/// treats that as `RERR_SOCKETIO` (clientname.c:323-327).
#[cfg(unix)]
pub(in crate::daemon) fn inherited_socket_peer_addr() -> std::io::Result<SocketAddr> {
    let stdin = std::io::stdin();
    let sock = socket2::SockRef::from(&stdin);
    let peer = sock.peer_addr()?;
    Ok(peer.as_socket().map_or_else(
        || SocketAddr::new(REMOTE_SHELL_DEFAULT_ADDR, 0),
        |addr| SocketAddr::new(normalize_v4_mapped(addr.ip()), addr.port()),
    ))
}

/// Non-Unix stub: the inetd entry point is Unix-only (`is_stdin_socket()`
/// returns `false` on other platforms), so this is unreachable in practice and
/// exists to keep the module compiling everywhere.
#[cfg(not(unix))]
pub(in crate::daemon) fn inherited_socket_peer_addr() -> std::io::Result<SocketAddr> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "inetd-style stdin sockets are not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvGuard;
    use std::ffi::OsStr;
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// upstream: clientname.c:73-74 - `SSH_CONNECTION` carries four
    /// space-separated fields and only the first is the peer.
    #[test]
    fn env_value_truncates_at_the_first_space() {
        assert_eq!(
            parse_peer_env_value("192.0.2.7 54321 198.51.100.1 22"),
            Some(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)))
        );
    }

    #[test]
    fn env_value_accepts_a_bare_address() {
        assert_eq!(
            parse_peer_env_value("198.51.100.9"),
            Some(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9)))
        );
    }

    /// upstream passes `allow_scope = True` to `valid_ipaddr`, so a link-local
    /// address carrying an interface scope is usable.
    #[test]
    fn env_value_accepts_a_scoped_ipv6_address() {
        assert_eq!(
            parse_peer_env_value("fe80::1%eth0 22 fe80::2 22"),
            Some(IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap()))
        );
    }

    #[test]
    fn env_value_rejects_a_non_address() {
        assert_eq!(parse_peer_env_value("not-an-address 22"), None);
        assert_eq!(parse_peer_env_value(""), None);
    }

    /// upstream: `valid_ipaddr()` has no scope branch on the IPv4 path, so an
    /// IPv4 value with a `%` suffix is invalid and upstream falls through to
    /// the socket rather than trusting the variable.
    #[test]
    fn env_value_rejects_a_scope_on_an_ipv4_address() {
        assert_eq!(parse_peer_env_value("192.0.2.7%eth0 22"), None);
    }

    /// upstream: clientname.c:521-522 - the scope must be non-empty and
    /// alphanumeric (`!*s && s[-1] != '%'`).
    #[test]
    fn env_value_rejects_an_empty_or_non_alphanumeric_scope() {
        assert_eq!(parse_peer_env_value("fe80::1%"), None);
        assert_eq!(parse_peer_env_value("fe80::1%eth-0"), None);
    }

    /// upstream: clientname.c:476-483 - a scope is legal mid-address only after
    /// a `::` was seen; a full 8-group address may still carry one at the end.
    #[test]
    fn env_value_scope_placement_follows_upstream() {
        assert_eq!(parse_peer_env_value("1:2:3:4:5:6:7%eth0"), None);
        assert_eq!(
            parse_peer_env_value("1:2:3:4:5:6:7:8%eth0"),
            Some(IpAddr::V6("1:2:3:4:5:6:7:8".parse::<Ipv6Addr>().unwrap()))
        );
    }

    /// upstream reads IPv4 octets with `strtol`, so `010` and `+2` pass
    /// `valid_ipaddr` and the daemon keeps the value instead of consulting the
    /// socket. The string is not a numeric host `getaddrinfo` reads the way
    /// `IpAddr` does, so it must match no address rule: the sentinel.
    #[test]
    fn env_value_accepts_strtol_octets_as_an_unmatchable_peer() {
        assert_eq!(
            parse_peer_env_value("010.1.2.3 22"),
            Some(REMOTE_SHELL_DEFAULT_ADDR)
        );
        assert_eq!(
            parse_peer_env_value("1.+2.3.4"),
            Some(REMOTE_SHELL_DEFAULT_ADDR)
        );
        assert_eq!(parse_peer_env_value("1.2.3.1000"), None);
        assert_eq!(parse_peer_env_value("1.2.3.256"), None);
        assert_eq!(parse_peer_env_value("1.2.3"), None);
        assert_eq!(parse_peer_env_value("1.2.3.4.5"), None);
    }

    /// upstream: clientname.c:71 - `strlcpy` into the 100-byte buffer drops
    /// everything past byte 99, so an over-long scope is cut back to valid.
    #[test]
    fn env_value_is_truncated_to_upstreams_buffer() {
        let mut value = String::from("fe80::1%");
        value.push_str(&"a".repeat(IPADDR_BUF_LEN - 1 - value.len()));
        value.push('-');
        assert_eq!(
            parse_peer_env_value(&value),
            Some(IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap()))
        );
    }

    /// upstream: clientname.c:478-485 - a dotted quad may only fill the last
    /// 32 bits of an IPv6 address.
    #[test]
    fn env_value_ipv6_with_embedded_ipv4_follows_upstream() {
        assert_eq!(
            parse_peer_env_value("::ffff:192.0.2.1"),
            Some(IpAddr::V6("::ffff:192.0.2.1".parse::<Ipv6Addr>().unwrap()))
        );
        assert_eq!(parse_peer_env_value("1:2:3:192.0.2.1"), None);
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
        assert_eq!(
            remote_shell_peer_addr(),
            RemoteShellPeer::Addr(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)))
        );
    }

    /// upstream: clientname.c:65 - with no variable set the seed survives.
    #[test]
    fn no_peer_environment_yields_the_unspecified_seed() {
        let _env_lock = crate::test_env::ENV_LOCK.lock().expect("lock environment");
        let _guards = REMOTE_SHELL_PEER_ENV.map(EnvGuard::remove);
        assert_eq!(
            remote_shell_peer_addr(),
            RemoteShellPeer::Addr(REMOTE_SHELL_DEFAULT_ADDR)
        );
    }

    /// upstream: clientname.c:66 - the FIRST set variable wins even when it is
    /// invalid; a later valid one is never consulted.
    #[test]
    fn an_invalid_first_variable_is_unusable_even_if_a_later_one_parses() {
        let _env_lock = crate::test_env::ENV_LOCK.lock().expect("lock environment");
        let _remote_host = EnvGuard::set("REMOTE_HOST", OsStr::new("not-an-address"));
        let _ssh_connection =
            EnvGuard::set("SSH_CONNECTION", OsStr::new("10.1.2.3 5555 10.0.0.1 22"));
        assert_eq!(remote_shell_peer_addr(), RemoteShellPeer::Unusable);
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
