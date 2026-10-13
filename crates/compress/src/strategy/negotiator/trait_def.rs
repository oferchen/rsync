//! [`CompressionNegotiator`] trait definition.

/// Trait for compression algorithm negotiation and selection.
///
/// Abstracts the algorithm preference ordering and mutual selection logic,
/// enabling alternative negotiation strategies (e.g., bandwidth-adaptive
/// selection, testing with fixed algorithms).
///
/// The wire-level vstring I/O remains in the `protocol` crate; this trait
/// only governs the selection decision once algorithm lists have been
/// exchanged.
///
/// # Upstream reference
///
/// upstream: compat.c:333-364 `parse_negotiate_str()` - each side walks the
/// peer's list and keeps the entry ranked highest in its own list.
///
/// Deviation from upstream: implementations here use the 3.4.x rule, where the
/// server stops at the first client entry it supports (`|| am_server`), so both
/// sides converge on the client's preference. 3.5.1 dropped that clause.
pub trait CompressionNegotiator: Send + Sync {
    /// Returns the ordered list of supported compression algorithm names.
    ///
    /// The first entry is the most preferred. This list is advertised to the
    /// remote peer during vstring exchange.
    ///
    /// # Upstream reference
    ///
    /// upstream: compat.c:101-112 `valid_compressions_items[]`
    fn supported_algorithms(&self) -> Vec<&'static str>;

    /// Selects the best mutual algorithm given the remote peer's advertised list.
    ///
    /// Uses the asymmetric selection rule of upstream rsync 3.4.x:
    /// - Server (`is_server=true`): iterates the remote (client's) list, returns
    ///   the first entry that also appears in the local list.
    /// - Client (`is_server=false`): iterates the local list, returns the first
    ///   entry that also appears in the remote (server's) list.
    ///
    /// Returns `"none"` if no mutual algorithm is found.
    ///
    /// # Upstream reference
    ///
    /// upstream: compat.c:333-364 `parse_negotiate_str()`
    fn select_algorithm(&self, remote_list: &[&str], is_server: bool) -> &'static str;
}
