//! IPv6-aware DNS resolution with `IpPreference` filtering.
//!
//! Resolves hostnames to socket addresses via `tokio::net::lookup_host`,
//! then filters or reorders results based on the configured IP version
//! preference. This mirrors the behavior of OpenSSH's `-4` and `-6` flags.

use std::net::SocketAddr;

use super::error::SshError;
use super::types::IpPreference;

/// Resolves a hostname and port to a list of socket addresses filtered by IP version preference.
///
/// Uses `tokio::net::lookup_host` for async DNS resolution. The returned addresses are
/// filtered or sorted according to `preference`:
///
/// - `Auto` - returns addresses in resolver order.
/// - `PreferV6` - sorts IPv6 addresses before IPv4.
/// - `ForceV4` - returns only IPv4 addresses.
/// - `ForceV6` - returns only IPv6 addresses.
///
/// # Errors
///
/// Returns `SshError::Io` if DNS resolution fails, or `SshError::DnsResolution` if the
/// resolver returned addresses but none matched the IP version preference.
pub async fn resolve_host(
    host: &str,
    port: u16,
    preference: IpPreference,
) -> Result<Vec<SocketAddr>, SshError> {
    let lookup_str = format!("{host}:{port}");
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host(&lookup_str).await?.collect();

    filter_by_preference(resolved, host, preference)
}

/// Filters and sorts a list of socket addresses according to IP version preference.
///
/// Extracted from `resolve_host` to enable deterministic unit testing without
/// actual DNS resolution.
fn filter_by_preference(
    addrs: Vec<SocketAddr>,
    host: &str,
    preference: IpPreference,
) -> Result<Vec<SocketAddr>, SshError> {
    let filtered = match preference {
        IpPreference::Auto => addrs,
        IpPreference::PreferV6 => {
            let mut sorted = addrs;
            sorted.sort_by_key(|addr| !addr.is_ipv6());
            sorted
        }
        IpPreference::ForceV4 => addrs.into_iter().filter(|a| a.is_ipv4()).collect(),
        IpPreference::ForceV6 => addrs.into_iter().filter(|a| a.is_ipv6()).collect(),
    };

    if filtered.is_empty() {
        let pref_label = match preference {
            IpPreference::Auto => "any",
            IpPreference::PreferV6 => "IPv6-preferred",
            IpPreference::ForceV4 => "IPv4",
            IpPreference::ForceV6 => "IPv6",
        };
        return Err(SshError::DnsResolution {
            host: host.to_owned(),
            preference: pref_label.to_owned(),
        });
    }

    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

    use super::*;

    fn v4(a: u8, b: u8, c: u8, d: u8, port: u16) -> SocketAddr {
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), port))
    }

    fn v6_loopback(port: u16) -> SocketAddr {
        SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, port, 0, 0))
    }

    fn v6(segments: [u16; 8], port: u16) -> SocketAddr {
        SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::new(
                segments[0],
                segments[1],
                segments[2],
                segments[3],
                segments[4],
                segments[5],
                segments[6],
                segments[7],
            ),
            port,
            0,
            0,
        ))
    }

    fn mixed_addrs() -> Vec<SocketAddr> {
        vec![
            v4(192, 168, 1, 1, 22),
            v6_loopback(22),
            v4(10, 0, 0, 1, 22),
            v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1], 22),
        ]
    }

    #[test]
    fn auto_returns_all_in_original_order() {
        let addrs = mixed_addrs();
        let original = addrs.clone();
        let result = filter_by_preference(addrs, "host", IpPreference::Auto).unwrap();
        assert_eq!(result, original);
    }

    #[test]
    fn force_v4_removes_ipv6() {
        let result = filter_by_preference(mixed_addrs(), "host", IpPreference::ForceV4).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|a| a.is_ipv4()));
        assert_eq!(result[0], v4(192, 168, 1, 1, 22));
        assert_eq!(result[1], v4(10, 0, 0, 1, 22));
    }

    #[test]
    fn force_v6_removes_ipv4() {
        let result = filter_by_preference(mixed_addrs(), "host", IpPreference::ForceV6).unwrap();
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|a| a.is_ipv6()));
    }

    #[test]
    fn prefer_v6_sorts_ipv6_first() {
        let result = filter_by_preference(mixed_addrs(), "host", IpPreference::PreferV6).unwrap();
        assert_eq!(result.len(), 4);
        // First two should be IPv6
        assert!(result[0].is_ipv6());
        assert!(result[1].is_ipv6());
        // Last two should be IPv4
        assert!(result[2].is_ipv4());
        assert!(result[3].is_ipv4());
    }

    #[test]
    fn prefer_v6_preserves_relative_order_within_family() {
        let result = filter_by_preference(mixed_addrs(), "host", IpPreference::PreferV6).unwrap();
        // IPv6 addresses preserve resolver order among themselves
        assert_eq!(result[0], v6_loopback(22));
        assert_eq!(result[1], v6([0x2001, 0xdb8, 0, 0, 0, 0, 0, 1], 22));
        // IPv4 addresses preserve resolver order among themselves
        assert_eq!(result[2], v4(192, 168, 1, 1, 22));
        assert_eq!(result[3], v4(10, 0, 0, 1, 22));
    }

    #[test]
    fn force_v4_error_when_only_ipv6() {
        let addrs = vec![v6_loopback(22)];
        let err = filter_by_preference(addrs, "example.com", IpPreference::ForceV4).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("IPv4"), "got: {msg}");
        assert!(msg.contains("example.com"), "got: {msg}");
    }

    #[test]
    fn force_v6_error_when_only_ipv4() {
        let addrs = vec![v4(127, 0, 0, 1, 22)];
        let err = filter_by_preference(addrs, "example.com", IpPreference::ForceV6).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("IPv6"), "got: {msg}");
        assert!(msg.contains("example.com"), "got: {msg}");
    }

    #[test]
    fn empty_input_returns_error_for_auto() {
        let err = filter_by_preference(vec![], "host", IpPreference::Auto).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("host"), "got: {msg}");
    }

    #[test]
    fn empty_input_returns_error_for_force_v4() {
        let err = filter_by_preference(vec![], "host", IpPreference::ForceV4).unwrap_err();
        assert!(err.to_string().contains("IPv4"));
    }

    #[test]
    fn empty_input_returns_error_for_force_v6() {
        let err = filter_by_preference(vec![], "host", IpPreference::ForceV6).unwrap_err();
        assert!(err.to_string().contains("IPv6"));
    }

    #[test]
    fn single_v4_with_auto() {
        let addrs = vec![v4(10, 0, 0, 1, 443)];
        let result = filter_by_preference(addrs.clone(), "h", IpPreference::Auto).unwrap();
        assert_eq!(result, addrs);
    }

    #[test]
    fn single_v6_with_force_v6() {
        let addrs = vec![v6_loopback(22)];
        let result = filter_by_preference(addrs.clone(), "h", IpPreference::ForceV6).unwrap();
        assert_eq!(result, addrs);
    }

    #[test]
    fn single_v4_with_force_v4() {
        let addrs = vec![v4(1, 2, 3, 4, 80)];
        let result = filter_by_preference(addrs.clone(), "h", IpPreference::ForceV4).unwrap();
        assert_eq!(result, addrs);
    }

    #[test]
    fn prefer_v6_with_only_v4_still_returns_v4() {
        let addrs = vec![v4(192, 168, 0, 1, 22), v4(10, 0, 0, 1, 22)];
        let result = filter_by_preference(addrs.clone(), "h", IpPreference::PreferV6).unwrap();
        assert_eq!(result, addrs);
    }

    #[test]
    fn prefer_v6_with_only_v6_returns_all() {
        let addrs = vec![v6_loopback(22), v6([0xfe80, 0, 0, 0, 0, 0, 0, 1], 22)];
        let result = filter_by_preference(addrs.clone(), "h", IpPreference::PreferV6).unwrap();
        assert_eq!(result, addrs);
    }

    #[test]
    fn port_preserved_through_filtering() {
        let addrs = vec![v4(1, 2, 3, 4, 2222), v6_loopback(2222)];
        let result = filter_by_preference(addrs, "h", IpPreference::ForceV4).unwrap();
        assert_eq!(result[0].port(), 2222);
    }

    #[test]
    fn dns_resolution_error_variant_fields() {
        let err = SshError::DnsResolution {
            host: "test.example".to_owned(),
            preference: "IPv6".to_owned(),
        };
        let msg = err.to_string();
        assert!(msg.contains("test.example"), "got: {msg}");
        assert!(msg.contains("IPv6"), "got: {msg}");
    }

    #[tokio::test]
    async fn resolve_host_localhost_auto() {
        // Localhost should always resolve on any platform.
        let result = resolve_host("localhost", 22, IpPreference::Auto).await;
        match result {
            Ok(addrs) => assert!(!addrs.is_empty()),
            Err(SshError::Io(_)) => {
                // Some CI environments may not resolve localhost - acceptable.
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn resolve_host_ipv4_literal() {
        let result = resolve_host("127.0.0.1", 22, IpPreference::Auto).await;
        match result {
            Ok(addrs) => {
                assert!(!addrs.is_empty());
                assert!(addrs[0].is_ipv4());
                assert_eq!(addrs[0].port(), 22);
            }
            Err(SshError::Io(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn resolve_host_ipv6_literal() {
        let result = resolve_host("::1", 22, IpPreference::ForceV6).await;
        match result {
            Ok(addrs) => {
                assert!(!addrs.is_empty());
                assert!(addrs.iter().all(|a| a.is_ipv6()));
            }
            // Some CI environments lack IPv6 - acceptable.
            Err(SshError::DnsResolution { .. }) | Err(SshError::Io(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn resolve_host_force_v4_filters_ipv6_from_localhost() {
        let result = resolve_host("localhost", 22, IpPreference::ForceV4).await;
        match result {
            Ok(addrs) => {
                assert!(addrs.iter().all(|a| a.is_ipv4()));
            }
            Err(SshError::DnsResolution { .. }) | Err(SshError::Io(_)) => {}
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[tokio::test]
    async fn resolve_host_nonexistent_returns_io_error() {
        let result = resolve_host("this-host-does-not-exist.invalid", 22, IpPreference::Auto).await;
        assert!(result.is_err());
    }
}
