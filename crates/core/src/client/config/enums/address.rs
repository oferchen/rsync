/// Selects the preferred address family for daemon and remote-shell connections.
///
/// When [`AddressMode::Ipv4`] or [`AddressMode::Ipv6`] is selected, network
/// operations restrict socket resolution to the requested family, mirroring
/// upstream rsync's `--ipv4` and `--ipv6` flags. The default mode allows the
/// operating system to pick whichever address family resolves first.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[doc(alias = "--ipv4")]
#[doc(alias = "--ipv6")]
#[derive(Default)]
pub enum AddressMode {
    /// Allow the operating system to pick the address family.
    #[default]
    Default,
    /// Restrict resolution and connections to IPv4 addresses.
    Ipv4,
    /// Restrict resolution and connections to IPv6 addresses.
    Ipv6,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_default_variant() {
        assert_eq!(AddressMode::default(), AddressMode::Default);
    }

    #[test]
    fn clone_and_copy() {
        let mode = AddressMode::Ipv4;
        let cloned = mode;
        let copied = mode;
        assert_eq!(mode, cloned);
        assert_eq!(mode, copied);
    }

    #[test]
    fn debug_format() {
        assert_eq!(format!("{:?}", AddressMode::Default), "Default");
        assert_eq!(format!("{:?}", AddressMode::Ipv4), "Ipv4");
        assert_eq!(format!("{:?}", AddressMode::Ipv6), "Ipv6");
    }
}
