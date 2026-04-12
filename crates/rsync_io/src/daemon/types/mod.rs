//! Legacy daemon handshake types for `@RSYNCD:` protocol negotiation.

mod handshake;
mod parts;

pub use handshake::LegacyDaemonHandshake;
pub use parts::LegacyDaemonHandshakeParts;
