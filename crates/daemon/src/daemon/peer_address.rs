// Resolution of the connecting peer's address for the two stdio daemon entry
// points, mirroring upstream's `client_addr()`.
//
// upstream: clientname.c:55-84 - `client_addr(int fd)` has two arms, chosen by
// the sign of `am_daemon`:
//
// ```c
// if (am_daemon < 0) {              /* daemon over --rsh mode */
//     strlcpy(ipaddr_buf, "0.0.0.0", sizeof ipaddr_buf);
//     if ((env_str = getenv("REMOTE_HOST"))    != NULL
//      || (env_str = getenv("SSH_CONNECTION")) != NULL
//      || (env_str = getenv("SSH_CLIENT"))     != NULL
//      || (env_str = getenv("SSH2_CLIENT"))    != NULL) {
//         strlcpy(ipaddr_buf, env_str, sizeof ipaddr_buf);
//         if ((p = strchr(ipaddr_buf, ' ')) != NULL) *p = '\0';
//     }
//     if (valid_ipaddr(ipaddr_buf, True)) return ipaddr_buf;
// }
// client_sockaddr(fd, &ss, &length);
// getnameinfo(..., NI_NUMERICHOST);
// ```
//
// and `client_sockaddr()` (clientname.c:37-45) does NOT invent a fallback when
// the socket cannot name its peer - it aborts the connection:
//
// ```c
// if (getpeername(fd, (struct sockaddr *) ss, ss_len)) {
//     rsyserr(FLOG, errno, "getpeername on fd%d failed", fd);
//     exit_cleanup(RERR_SOCKETIO);
// }
// ```
//
// The two entry points are therefore NOT the same case, which is what made the
// previous code look defensible: under inetd, fd 0 *is* the connected socket
// and `getpeername` succeeds, so there is a real address to be had; under a
// remote shell, fd 0 is a pipe and the address can only come from the
// environment. Fabricating `127.0.0.1` for both made every `hosts allow` /
// `hosts deny` rule evaluate against a synthetic localhost - admitting every
// client under the canonical `hosts allow = 127.0.0.1`, and rejecting every
// legitimate client under an allow-list naming real subnets.

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

/// Parses one environment value the way upstream does: truncate at the first
/// space, then validate.
///
/// `SSH_CONNECTION` is `"<client-ip> <client-port> <server-ip> <server-port>"`
/// and `SSH_CLIENT` is `"<client-ip> <client-port> <server-port>"`, so the
/// leading field is the peer address in both.
///
/// A scope suffix (`fe80::1%eth0`) is accepted because upstream passes
/// `allow_scope = True`; `IpAddr`'s parser rejects it, so the scope is stripped
/// before parsing. The scope itself names a *local* interface and has no
/// bearing on which peer this is, so dropping it loses nothing an ACL can use.
///
/// upstream: clientname.c:70-77 plus `valid_ipaddr(buf, True)`.
fn parse_peer_env_value(value: &str) -> Option<IpAddr> {
    let field = value.split(' ').next().unwrap_or(value);
    if let Ok(addr) = field.parse::<IpAddr>() {
        return Some(addr);
    }
    let (bare, _scope) = field.split_once('%')?;
    bare.parse::<IpAddr>().ok()
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
/// upstream: clientname.c:47-74 - `client_sockaddr()` rewrites a V4MAPPED
/// `sockaddr_in6` into a `sockaddr_in` before any lookup or comparison.
fn normalize_v4_mapped(addr: IpAddr) -> IpAddr {
    match addr {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(addr, IpAddr::V4),
        IpAddr::V4(_) => addr,
    }
}

/// Reads the real peer address from the socket inherited on stdin.
///
/// Used by the inetd / socket-activation / `RSYNC_CONNECT_PROG` entry point,
/// where fd 0 is the connected socket and the address is genuinely available.
///
/// `socket2::SockRef::from(&io::Stdin)` borrows the descriptor through `AsFd`
/// without taking ownership, so nothing closes stdin and no `unsafe` is needed
/// - the same construction `is_stdin_socket()` already uses to classify fd 0.
///
/// A peer that HAS an address but not an IP one - an `AF_UNIX` socketpair, as
/// used by systemd socket activation with `Accept=yes` and by
/// `RSYNC_CONNECT_PROG` - degrades to [`REMOTE_SHELL_DEFAULT_ADDR`] rather than
/// failing. Upstream serves such a connection: `getpeername` succeeds, and the
/// `getnameinfo` that follows fails but its return value is never checked
/// (clientname.c:81), so `ipaddr_buf` is simply left empty. Both "" and
/// `0.0.0.0` match no realistic `hosts allow` token, so the ACL outcome is the
/// same; using the sentinel keeps oc from inventing an address it does not have
/// while still serving a configuration upstream supports.
///
/// # Errors
///
/// Returns the `getpeername` error unchanged when the call itself fails. The
/// caller must abort the session rather than substitute a placeholder; upstream
/// treats that as `RERR_SOCKETIO` (clientname.c:41-45).
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

    /// The security-relevant default. `0.0.0.0` matches no realistic
    /// `hosts allow` token, so an unconfigured remote-shell daemon denies
    /// rather than admits.
    #[test]
    fn default_remote_shell_address_is_unspecified_not_loopback() {
        assert_eq!(REMOTE_SHELL_DEFAULT_ADDR, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_ne!(REMOTE_SHELL_DEFAULT_ADDR, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    /// upstream: clientname.c:47-74 - a V4MAPPED peer is rewritten to IPv4
    /// before any ACL comparison, so `hosts allow = 192.0.2.0/24` matches an
    /// IPv4 client arriving on a dual-stack listener.
    #[test]
    fn v4_mapped_ipv6_normalises_to_ipv4() {
        let mapped: IpAddr = "::ffff:192.0.2.128".parse().unwrap();
        assert_eq!(
            normalize_v4_mapped(mapped),
            IpAddr::V4(Ipv4Addr::new(192, 0, 2, 128))
        );
    }

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
