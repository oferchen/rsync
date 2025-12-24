mod negotiate;
mod types;

pub use negotiate::{
    negotiate_legacy_daemon_session, negotiate_legacy_daemon_session_from_stream,
    negotiate_legacy_daemon_session_with_sniffer,
};
pub use types::{LegacyDaemonHandshake, LegacyDaemonHandshakeParts};

#[cfg(test)]
mod tests;
