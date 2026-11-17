//! Filter options on a remote-shell server's own argv.
//!
//! upstream: options.c:1647-1683 and :1718-1727 - the server parses them in
//! the same `parse_arguments()` as the client, relative to its cwd and
//! unsanitized (only a daemon module sets `sanitize_paths`,
//! clientserver.c:1068). They land in `filter_list` ahead of the rules
//! `recv_filter_list()` then appends from the wire (exclude.c:1971-1986).

use std::ffi::OsString;
use std::io::Write;

use core::message::{Message, Role};
use protocol::filters::FilterRuleWireFormat;

use crate::frontend::filter_rules::ProcessFilterFiles;

/// Builds the rules the filter options in a remote-shell server's normalized
/// argv add, reading files the way the client does.
///
/// # Errors
///
/// The first malformed rule or unreadable filter file.
pub(super) fn server_argv_filter_rules(
    args: &[OsString],
    from0: bool,
) -> Result<Vec<FilterRuleWireFormat>, Message> {
    core::client::filter_args::server_argv_filter_rules(args, from0, &ProcessFilterFiles)
}

/// Prints a refused server-argv filter as upstream does and returns its exit.
///
/// upstream: the server parses these before the roles split, where
/// who_am_i() is "server" (rsync.c:989-990).
pub(super) fn write_filter_refusal<Err: Write>(
    stderr: &mut Err,
    brand: core::branding::Brand,
    message: &Message,
) -> i32 {
    core::client::filter_args::write_refusal(stderr, brand, Role::Server, message)
        .unwrap_or_else(|_| message.code().unwrap_or(1))
}
