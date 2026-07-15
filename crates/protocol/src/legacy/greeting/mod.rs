//! Legacy daemon greeting parsing, formatting, and structured representations.
//!
//! Splits the `@RSYNCD: <version>` greeting helpers across focused submodules:
//! `format` renders the canonical banner, `parse` validates incoming bytes,
//! `tokens` iterates digest lists, and `types` exposes borrowed and owned views
//! of the parsed metadata.

mod format;
mod parse;
mod tokens;
mod types;
mod validate;

pub use format::{format_legacy_daemon_greeting, write_legacy_daemon_greeting};
pub use parse::{
    parse_legacy_daemon_greeting, parse_legacy_daemon_greeting_details,
    parse_legacy_daemon_greeting_owned,
};
pub use tokens::DigestListTokens;
pub use types::{LegacyDaemonGreeting, LegacyDaemonGreetingOwned};
pub use validate::{MissingGreetingToken, missing_greeting_token};

#[cfg(test)]
mod tests;
