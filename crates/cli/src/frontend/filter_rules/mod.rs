//! Collecting the CLI's filter options in argv order.
//!
//! The rules themselves are built by `core::client::filter_args`, the one
//! parser the client and a remote-shell server share.

mod arguments;
mod files;

pub(crate) use arguments::build_filter_order;
pub(crate) use core::client::filter_args::FilterOrderToken;
pub(crate) use files::ProcessFilterFiles;

#[cfg(test)]
pub(crate) use files::set_filter_stdin_input;
