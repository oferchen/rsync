mod core;
mod drain;
mod legacy;
mod observe;
mod util;

#[cfg(feature = "async")]
mod async_read;

pub use core::NegotiationPrologueSniffer;
pub use legacy::{
    read_and_parse_legacy_daemon_greeting, read_and_parse_legacy_daemon_greeting_details,
    read_legacy_daemon_line,
};

pub(crate) use legacy::map_reserve_error_for_io;
