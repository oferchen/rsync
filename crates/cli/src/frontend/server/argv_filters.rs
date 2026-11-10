//! Filter options on a remote-shell server's own argv.
//!
//! upstream: options.c:1620-1656 and :1718-1727 - the server parses them in
//! the same `parse_arguments()` as the client, relative to its cwd and
//! unsanitized (only a daemon module sets `sanitize_paths`,
//! clientserver.c:1068). They land in `filter_list` ahead of the rules
//! `recv_filter_list()` then appends from the wire (exclude.c:1971-1986).

use std::ffi::OsString;
use std::io::Write;

use core::message::{Message, Role};
use protocol::filters::FilterRuleWireFormat;

use crate::frontend::execution::exit_trailer;
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

/// The `RERR_FILEIO` exit a filter file that fails to open ends with.
///
/// upstream: exclude.c:1719 `exit_cleanup(RERR_FILEIO)`.
const FILE_IO_EXIT: i32 = 11;

/// Prints a refused server-argv filter as 3.5.1 does and returns its exit.
///
/// upstream: a failed open is `rsyserr()` (exclude.c:1714-1717), whose line
/// carries `[server]` (log.c:490, who_am_i() before the roles split); a
/// malformed rule is filter_rule_err()'s bare line (exclude.c:132-136). Both
/// then exit through log_exit()'s trailer.
pub(super) fn write_filter_refusal<Err: Write>(
    stderr: &mut Err,
    brand: core::branding::Brand,
    message: &Message,
) -> i32 {
    let code = message.code().unwrap_or(1);
    let _ = if code == FILE_IO_EXIT {
        writeln!(
            stderr,
            "{}: [server] {}",
            brand.client_program_name(),
            message.text()
        )
    } else {
        writeln!(stderr, "{}", message.text())
    };
    if let Some(template) = core::message::strings::exit_code_message(code) {
        let trailer = exit_trailer(template, Role::Server);
        let mut sink = logging_sink::MessageSink::with_brand(&mut *stderr, brand);
        if super::super::write_message(&trailer, &mut sink).is_err() {
            let _ = writeln!(sink.writer_mut(), "{trailer}");
        }
    }
    code
}
