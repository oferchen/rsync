//! Network interface name and index lookups.
//!
//! upstream: an IPv6 peer string such as `fe80::1%eth0` carries its scope as
//! an interface name; `getaddrinfo()` turns that name into the
//! `sin6_scope_id` index and `getnameinfo()` turns the index back into the
//! name. These helpers expose the two directions of that mapping.

/// Returns the index of the interface named `name`, or `None` when no
/// interface has that name.
#[cfg(unix)]
pub fn interface_index(name: &str) -> Option<u32> {
    nix::net::if_::if_nametoindex(name)
        .ok()
        .filter(|&index| index != 0)
}

/// Returns the name of the interface with index `index`, or `None` when no
/// interface has that index or its name is not UTF-8.
#[cfg(unix)]
pub fn interface_name(index: u32) -> Option<String> {
    // nix 0.31 reports index 0 on glibc as an empty name rather than an error.
    nix::net::if_::if_indextoname(index)
        .ok()
        .and_then(|name| name.into_string().ok())
        .filter(|name| !name.is_empty())
}

/// Returns the index of the interface named `name`; interface names are not
/// resolved on this platform, so this is always `None`.
#[cfg(not(unix))]
pub fn interface_index(_name: &str) -> Option<u32> {
    None
}

/// Returns the name of the interface with index `index`; interface names are
/// not resolved on this platform, so this is always `None`.
#[cfg(not(unix))]
pub fn interface_name(_index: u32) -> Option<String> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The loopback interface exists on every Unix host, so a name read back
    /// from its index must map to the same index again.
    #[test]
    fn loopback_index_and_name_round_trip() {
        let name = ["lo", "lo0"]
            .into_iter()
            .find(|name| interface_index(name).is_some())
            .expect("a loopback interface");
        let index = interface_index(name).expect("loopback index");
        assert_eq!(interface_name(index).as_deref(), Some(name));
    }

    #[test]
    fn unknown_names_and_indexes_resolve_to_none() {
        assert_eq!(interface_index("oc-no-such-if0"), None);
        assert_eq!(interface_index(""), None);
        assert_eq!(interface_name(0), None);
    }
}
