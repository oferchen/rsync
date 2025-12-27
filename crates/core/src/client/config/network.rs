use std::ffi::{OsStr, OsString};
use std::net::SocketAddr;

/// Describes a bind address specified via `--address`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindAddress {
    raw: OsString,
    socket: SocketAddr,
}

impl BindAddress {
    /// Creates a new bind address from the caller-provided specification.
    #[must_use]
    pub const fn new(raw: OsString, socket: SocketAddr) -> Self {
        Self { raw, socket }
    }

    /// Returns the raw command-line representation forwarded to the fallback binary.
    #[must_use]
    pub fn raw(&self) -> &OsStr {
        self.raw.as_os_str()
    }

    /// Returns the socket address (with port zero) used when binding local connections.
    #[must_use]
    pub const fn socket(&self) -> SocketAddr {
        self.socket
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    // Tests for BindAddress::new
    #[test]
    fn new_creates_bind_address() {
        let raw = OsString::from("192.168.1.1");
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
        let bind = BindAddress::new(raw.clone(), socket);
        assert_eq!(bind.raw(), raw.as_os_str());
        assert_eq!(bind.socket(), socket);
    }

    // Tests for BindAddress::raw
    #[test]
    fn raw_returns_original_string() {
        let raw = OsString::from("10.0.0.1");
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 0);
        let bind = BindAddress::new(raw.clone(), socket);
        assert_eq!(bind.raw(), "10.0.0.1");
    }

    // Tests for BindAddress::socket
    #[test]
    fn socket_returns_address() {
        let raw = OsString::from("127.0.0.1");
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let bind = BindAddress::new(raw, socket);
        assert_eq!(bind.socket().ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(bind.socket().port(), 0);
    }

    #[test]
    fn socket_with_ipv6() {
        let raw = OsString::from("::1");
        let socket = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 0);
        let bind = BindAddress::new(raw, socket);
        assert_eq!(bind.socket().ip(), IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    // Tests for trait implementations
    #[test]
    fn bind_address_is_clone() {
        let raw = OsString::from("192.168.1.1");
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
        let bind = BindAddress::new(raw, socket);
        let cloned = bind.clone();
        assert_eq!(bind, cloned);
    }

    #[test]
    fn bind_address_debug() {
        let raw = OsString::from("192.168.1.1");
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0);
        let bind = BindAddress::new(raw, socket);
        let debug = format!("{bind:?}");
        assert!(debug.contains("BindAddress"));
    }

    #[test]
    fn bind_addresses_with_same_values_are_equal() {
        let bind1 = BindAddress::new(
            OsString::from("192.168.1.1"),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0),
        );
        let bind2 = BindAddress::new(
            OsString::from("192.168.1.1"),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0),
        );
        assert_eq!(bind1, bind2);
    }

    #[test]
    fn bind_addresses_with_different_raw_are_not_equal() {
        let bind1 = BindAddress::new(
            OsString::from("192.168.1.1"),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0),
        );
        let bind2 = BindAddress::new(
            OsString::from("different"),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0),
        );
        assert_ne!(bind1, bind2);
    }

    #[test]
    fn bind_addresses_with_different_socket_are_not_equal() {
        let bind1 = BindAddress::new(
            OsString::from("192.168.1.1"),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 0),
        );
        let bind2 = BindAddress::new(
            OsString::from("192.168.1.1"),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 0),
        );
        assert_ne!(bind1, bind2);
    }
}
