//! Session handshake wrapper unifying binary and legacy daemon negotiations.
//!
//! The [`SessionHandshake`] enum abstracts over the two rsync negotiation
//! styles - binary remote-shell (protocols >= 30) and legacy `@RSYNCD:` daemon -
//! exposing a uniform API for protocol metadata, stream access, variant
//! downcasting, and parts decomposition.
//!
//! Implementation is split across focused submodules:
//!
//! - [`protocol`] - protocol version query accessors
//! - [`stream`] - stream access, variant downcasts, and transport mapping
//! - [`parts`] - decomposition into / reassembly from parts, plus trait impls

mod parts;
mod protocol;
mod stream;

#[cfg(test)]
mod tests;

use crate::binary::BinaryHandshake;
use crate::daemon::LegacyDaemonHandshake;

/// Result of negotiating an rsync session over an arbitrary transport.
///
/// The enum wraps either the binary remote-shell handshake or the legacy ASCII
/// daemon negotiation while exposing convenience accessors that mirror the
/// per-variant helpers. Higher layers can match on the
/// [`SessionHandshake::decision`] to branch on
/// the negotiated style without re-sniffing the transport. Conversions are
/// provided via [`From`] and [`TryFrom`] so variant-specific wrappers can be
/// promoted or recovered ergonomically.
///
/// When the underlying transport implements [`Clone`], the session wrapper can
/// also be cloned. The clone retains the negotiated metadata and replay buffer
/// so both instances may continue processing without interfering with each
/// other - useful for tooling that needs to inspect the transcript while keeping
/// the original session active.
#[derive(Clone, Debug)]
pub enum SessionHandshake<R> {
    /// Binary remote-shell style negotiation (protocols >= 30).
    Binary(BinaryHandshake<R>),
    /// Legacy `@RSYNCD:` daemon negotiation.
    #[doc(alias = "@RSYNCD")]
    Legacy(LegacyDaemonHandshake<R>),
}
