//! Peer-address discovery for daemon sessions that did not come from `accept()`.
//!
//! A TCP daemon learns its peer from the accepted socket. The two stdio modes
//! cannot, and upstream answers the question differently for each:
//!
//! - **inetd** (`am_daemon > 0`): `start_daemon(STDIN_FILENO, STDIN_FILENO)`
//!   (`clientserver.c:1559`) hands the CONNECTED SOCKET in as fd 0, so
//!   `client_addr()` skips the environment entirely and calls
//!   `client_sockaddr()` -> `getpeername()` (`clientname.c:80`).
//! - **remote shell** (`am_daemon < 0`): there is no socket, only a pipe from
//!   the shell, so `client_addr()` seeds `"0.0.0.0"` and then consults four
//!   environment variables the shell may have set (`clientname.c:63-77`).
//!
//! Both fall back to `0.0.0.0`, never to localhost. That matters: an unknown
//! peer must match no `hosts allow` token so access control fails CLOSED.
//! Reporting `127.0.0.1` makes `hosts allow = 127.0.0.1` - the canonical
//! "local connections only" rule - admit every client.
//!
//! This module contains no `unsafe`: `Stdin` implements `AsFd` and
//! `socket2::SockRef` borrows any `AsFd` safely, so `crates/daemon` keeps its
//! crate-level `deny(unsafe_code)` without routing through a wrapper crate.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Upstream's seed value for a peer whose address cannot be determined.
///
/// upstream: `clientname.c:64` - `strlcpy(ipaddr_buf, "0.0.0.0", ...)`.
pub const UNKNOWN_PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);

/// The environment variables a remote shell may use to publish the client's
/// address, in upstream's exact precedence order.
///
/// upstream: `clientname.c:66-69` - a single `if` chain of `getenv()` calls, so
/// the FIRST variable that is set wins even if its value is unusable.
const PEER_ADDRESS_VARS: &[&str] = &["REMOTE_HOST", "SSH_CONNECTION", "SSH_CLIENT", "SSH2_CLIENT"];

/// Peer address of the socket inherited on stdin, when there is one.
///
/// Returns `None` when stdin is not a socket, which is the normal case for a
/// remote-shell daemon (stdin is a pipe) and for a hand-run `--server --daemon`.
///
/// upstream: `clientname.c:80` - `client_sockaddr(fd, &ss, &length)`.
#[cfg(unix)]
#[must_use]
pub fn inherited_peer_addr() -> Option<SocketAddr> {
    let stdin = std::io::stdin();
    socket2::SockRef::from(&stdin)
        .peer_addr()
        .ok()
        .and_then(|addr| addr.as_socket())
}

/// Windows has no inetd-style daemon mode, so there is never an inherited
/// socket to query.
#[cfg(not(unix))]
#[must_use]
pub fn inherited_peer_addr() -> Option<SocketAddr> {
    None
}

/// Extracts a peer IP from one environment value.
///
/// `SSH_CONNECTION` is `"<client-ip> <client-port> <server-ip> <server-port>"`
/// and `SSH_CLIENT` is `"<client-ip> <client-port> <server-port>"`, so upstream
/// truncates at the first space and keeps only the address. `REMOTE_HOST` is
/// already a bare address; truncating it is harmless.
///
/// upstream: `clientname.c:71-75` - `strlcpy` then
/// `if ((p = strchr(ipaddr_buf, ' ')) != NULL) *p = '\0';` then
/// `valid_ipaddr(ipaddr_buf, True)`.
fn parse_peer_ip(value: &str) -> Option<IpAddr> {
    value.split(' ').next()?.parse().ok()
}

/// Peer address for a remote-shell daemon, read from the environment.
///
/// Mirrors upstream's chain exactly: the first variable that is SET wins, and
/// if its value does not parse as an IP the address stays unknown rather than
/// falling through to the next variable. That "first set wins" detail is
/// load-bearing - upstream's `if (a) else if (b)` chain tests only whether
/// `getenv` returned non-NULL, not whether the value was usable.
///
/// upstream: `clientname.c:63-77`.
#[must_use]
pub fn remote_shell_peer_addr() -> SocketAddr {
    let Some(value) = PEER_ADDRESS_VARS
        .iter()
        .find_map(|name| std::env::var(name).ok())
    else {
        return UNKNOWN_PEER;
    };
    parse_peer_ip(&value).map_or(UNKNOWN_PEER, |ip| SocketAddr::new(ip, 0))
}

/// Which stdio start-up mode the daemon is in, and therefore how its peer is found.
///
/// upstream: `clientname.c:59-77` branches on the SIGN of `am_daemon` - a
/// socket-backed daemon queries the socket, a remote-shell daemon consults the
/// environment. Keeping the two apart is upstream-faithful, not an abstraction
/// for its own sake: collapsing them would give one mode the other's fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdioMode {
    /// inetd-style (`am_daemon > 0`): fd 0 IS the connected socket.
    Inetd,
    /// Remote shell (`am_daemon < 0`): fd 0 is a pipe, so no socket exists.
    RemoteShell,
}

/// Resolves the peer address for a daemon session started on stdio.
///
/// `inherited` is passed IN rather than discovered here, which makes the whole
/// decision a pure function: it is testable without a socket, and both call
/// sites collapse to a single expression. That is the point - the original
/// defect was a literal `127.0.0.1` written independently at each call site, and
/// a site that holds no address of its own cannot reintroduce one.
///
/// upstream: `clientname.c:63-80` - seed `0.0.0.0`, then either `getpeername`
/// or the environment chain depending on the mode; never localhost.
#[must_use]
pub fn stdio_peer_addr(mode: StdioMode, inherited: Option<SocketAddr>) -> SocketAddr {
    match (inherited, mode) {
        // A real socket answers for itself in either mode.
        (Some(peer), _) => peer,
        // inetd with no readable socket keeps upstream's seed.
        (None, StdioMode::Inetd) => UNKNOWN_PEER,
        (None, StdioMode::RemoteShell) => remote_shell_peer_addr(),
    }
}

#[cfg(test)]
mod tests {
    use super::{StdioMode, UNKNOWN_PEER, parse_peer_ip, stdio_peer_addr};
    use std::net::IpAddr;

    /// `SSH_CONNECTION` carries four space-separated fields; only the first is
    /// the client address. Without the truncation the whole string fails to
    /// parse and every ssh-spawned client looks unknown.
    ///
    /// upstream: `clientname.c:73-74`.
    #[test]
    fn truncates_at_the_first_space() {
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        assert_eq!(parse_peer_ip("10.1.2.3 54321 10.9.9.9 873"), Some(ip));
        assert_eq!(parse_peer_ip("10.1.2.3 54321 873"), Some(ip));
        assert_eq!(parse_peer_ip("10.1.2.3"), Some(ip));
    }

    /// IPv6 peers arrive in the same shape.
    #[test]
    fn accepts_ipv6() {
        assert_eq!(
            parse_peer_ip("2001:db8::1 54321 2001:db8::2 873"),
            Some("2001:db8::1".parse::<IpAddr>().unwrap()),
        );
    }

    /// A malformed or empty value must NOT become an address.
    ///
    /// upstream: `clientname.c:76` - the value is used only when
    /// `valid_ipaddr()` accepts it.
    #[test]
    fn rejects_unparseable_values() {
        for value in ["", " ", "not-an-ip", "example.com 22", "999.1.1.1"] {
            assert_eq!(parse_peer_ip(value), None, "value {value:?}");
        }
    }

    /// The unknown-peer fallback is upstream's `0.0.0.0`, NOT localhost.
    ///
    /// This is the whole security property: `0.0.0.0` matches no realistic
    /// `hosts allow` token, so an undeterminable peer is refused. Localhost
    /// matched the canonical "local only" rule and admitted everyone.
    ///
    /// upstream: `clientname.c:64`.
    #[test]
    fn unknown_peer_is_the_unspecified_address() {
        assert!(UNKNOWN_PEER.ip().is_unspecified());
        assert!(!UNKNOWN_PEER.ip().is_loopback());
    }

    /// An inetd daemon whose stdin is not a readable socket falls back to
    /// upstream's seed, NOT localhost.
    ///
    /// This is the call-site guard the composition tests in `host_pattern`
    /// cannot provide: they pin the CONSTANT, this pins the DECISION the inetd
    /// path actually makes. Deterministic - it consults no socket and no
    /// environment.
    #[test]
    fn inetd_without_a_socket_falls_back_to_the_unknown_peer() {
        let resolved = stdio_peer_addr(StdioMode::Inetd, None);
        assert_eq!(resolved, UNKNOWN_PEER);
        assert!(
            !resolved.ip().is_loopback(),
            "the inetd fallback must never be loopback - that is the fail-open"
        );
    }

    /// A genuine inherited socket wins in BOTH modes and is reported verbatim,
    /// so access control sees the real peer rather than any fallback.
    #[test]
    fn an_inherited_socket_wins_in_both_modes() {
        let peer: std::net::SocketAddr = "203.0.113.7:5555".parse().unwrap();
        for mode in [StdioMode::Inetd, StdioMode::RemoteShell] {
            assert_eq!(stdio_peer_addr(mode, Some(peer)), peer, "mode {mode:?}");
        }
    }

    /// A loopback peer is reported only when the socket genuinely says so - the
    /// fix must not over-correct into refusing real local clients.
    #[test]
    fn a_real_loopback_socket_is_still_reported() {
        let local: std::net::SocketAddr = "127.0.0.1:873".parse().unwrap();
        assert_eq!(stdio_peer_addr(StdioMode::Inetd, Some(local)), local);
    }
}
