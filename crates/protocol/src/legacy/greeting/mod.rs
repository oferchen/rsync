//! Legacy daemon greeting parsing, formatting, and structured representations.
//!
//! Splits the `@RSYNCD: <version>` greeting helpers across focused submodules:
//! `format` renders the canonical banner, `parse` validates incoming bytes,
//! `advertised` models the digest name list, `tokens` iterates it, and `types`
//! exposes borrowed and owned views of the parsed metadata.

mod advertised;
mod format;
mod parse;
mod tokens;
mod types;
mod validate;

pub use advertised::AdvertisedDigests;
pub use format::{
    DAEMON_AUTH_DIGEST_NAMES, daemon_auth_digest_list, format_legacy_daemon_greeting,
    write_daemon_auth_digest_list, write_legacy_daemon_greeting,
};
pub use parse::{
    parse_legacy_daemon_greeting, parse_legacy_daemon_greeting_details,
    parse_legacy_daemon_greeting_owned,
};
pub use tokens::DigestListTokens;
pub use types::{LegacyDaemonGreeting, LegacyDaemonGreetingOwned};
pub use validate::{
    MissingGreetingToken, advertised_digests_in_greeting, is_version_banner, missing_greeting_token,
};

#[cfg(test)]
mod tests;
