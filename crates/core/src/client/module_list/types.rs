use std::fmt;

use super::super::{ClientError, FEATURE_UNAVAILABLE_EXIT_CODE, daemon_error};

/// Target daemon address used for module listing requests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonAddress {
    pub(crate) host: String,
    pub(crate) port: u16,
}

impl DaemonAddress {
    /// Creates a new daemon address from the supplied host and port.
    pub fn new(host: String, port: u16) -> Result<Self, ClientError> {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(daemon_error(
                "daemon host must be non-empty",
                FEATURE_UNAVAILABLE_EXIT_CODE,
            ));
        }
        Ok(Self {
            host: trimmed.to_owned(),
            port,
        })
    }

    /// Returns the daemon host name or address.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the daemon TCP port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    pub(crate) fn socket_addr_display(&self) -> SocketAddrDisplay<'_> {
        SocketAddrDisplay {
            host: &self.host,
            port: self.port,
        }
    }
}

pub(crate) struct SocketAddrDisplay<'a> {
    pub(super) host: &'a str,
    pub(super) port: u16,
}

impl fmt::Display for SocketAddrDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') && !self.host.starts_with('[') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_address_new_creates_address() {
        let addr = DaemonAddress::new("localhost".to_owned(), 873).expect("address");
        assert_eq!(addr.host(), "localhost");
        assert_eq!(addr.port(), 873);
    }

    #[test]
    fn daemon_address_new_trims_whitespace() {
        let addr = DaemonAddress::new("  example.com  ".to_owned(), 8873).expect("address");
        assert_eq!(addr.host(), "example.com");
    }

    #[test]
    fn daemon_address_new_rejects_empty_host() {
        let result = DaemonAddress::new("".to_owned(), 873);
        assert!(result.is_err());
    }

    #[test]
    fn daemon_address_new_rejects_whitespace_only_host() {
        let result = DaemonAddress::new("   ".to_owned(), 873);
        assert!(result.is_err());
    }

    #[test]
    fn daemon_address_eq_works() {
        let a = DaemonAddress::new("localhost".to_owned(), 873).expect("a");
        let b = DaemonAddress::new("localhost".to_owned(), 873).expect("b");
        let c = DaemonAddress::new("localhost".to_owned(), 8873).expect("c");

        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn daemon_address_clone_works() {
        let addr = DaemonAddress::new("example.com".to_owned(), 873).expect("address");
        let cloned = addr.clone();
        assert_eq!(addr, cloned);
    }

    #[test]
    fn socket_addr_display_formats_simple_host() {
        let display = SocketAddrDisplay {
            host: "localhost",
            port: 873,
        };
        assert_eq!(format!("{display}"), "localhost:873");
    }

    #[test]
    fn socket_addr_display_brackets_ipv6() {
        let display = SocketAddrDisplay {
            host: "::1",
            port: 873,
        };
        assert_eq!(format!("{display}"), "[::1]:873");
    }

    #[test]
    fn socket_addr_display_does_not_double_bracket() {
        let display = SocketAddrDisplay {
            host: "[::1]",
            port: 873,
        };
        assert_eq!(format!("{display}"), "[::1]:873");
    }

    #[test]
    fn socket_addr_display_formats_hostname_with_port() {
        let display = SocketAddrDisplay {
            host: "example.com",
            port: 8873,
        };
        assert_eq!(format!("{display}"), "example.com:8873");
    }

    #[test]
    fn daemon_address_socket_addr_display_formats_correctly() {
        let addr = DaemonAddress::new("192.168.1.1".to_owned(), 873).expect("address");
        let display = addr.socket_addr_display();
        assert_eq!(format!("{display}"), "192.168.1.1:873");
    }
}
