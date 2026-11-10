//! Filter options on a remote-shell server's own argv.
//!
//! upstream: options.c:1620-1656 and :1718-1727 - the server parses them in
//! the same `parse_arguments()` as the client, relative to its cwd and
//! unsanitized (only a daemon module sets `sanitize_paths`,
//! clientserver.c:1068). They land in `filter_list` ahead of the rules
//! `recv_filter_list()` then appends from the wire (exclude.c:1971-1986).

use std::ffi::OsString;
use std::io::Write;

use core::client::remote::build_wire_format_rules;
use core::message::{Message, Role};
use protocol::filters::FilterRuleWireFormat;

use super::flags::{FILTER_OPTIONS, split_at_end_of_options};
use crate::frontend::execution::{FilterInputs, build_filter_rules, exit_trailer};
use crate::frontend::filter_rules::FilterOrderToken;

/// The `RERR_FILEIO` exit a filter file that fails to open ends with.
///
/// upstream: exclude.c:1719 `exit_cleanup(RERR_FILEIO)`.
const FILE_IO_EXIT: i32 = 11;

/// Builds the rules the filter options in a normalized server argv add.
///
/// `normalize_server_argv` has already joined every filter option to its
/// value and expanded each `-F`, so only `--opt=VALUE` tokens remain.
///
/// # Errors
///
/// The first malformed rule or unreadable filter file, as upstream reports
/// it before exiting.
pub(super) fn server_argv_filter_rules(
    args: &[OsString],
    from0: bool,
) -> Result<Vec<FilterRuleWireFormat>, Message> {
    let order = filter_order(args);
    if order.is_empty() {
        return Ok(Vec::new());
    }
    let cvs = filters::CvsSession::default();
    let specs = build_filter_rules(FilterInputs { order, from0 }, &cvs)?;
    build_wire_format_rules(&specs, false).map_err(|error| error.message().clone())
}

/// The filter options in a normalized server argv, in argv order.
fn filter_order(args: &[OsString]) -> Vec<FilterOrderToken> {
    let (options, _operands) = split_at_end_of_options(args);
    options
        .iter()
        .filter_map(|arg| {
            let text = arg.to_string_lossy();
            let (name, value) = text.split_once('=')?;
            let value = OsString::from(value);
            let token = match FILTER_OPTIONS.iter().position(|option| *option == name)? {
                0 => FilterOrderToken::Filter(value),
                1 => FilterOrderToken::Exclude(value),
                2 => FilterOrderToken::Include(value),
                3 => FilterOrderToken::ExcludeFrom(value),
                _ => FilterOrderToken::IncludeFrom(value),
            };
            Some(token)
        })
        .collect()
}

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
