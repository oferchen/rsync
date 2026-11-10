//! Filter options on a server's own argv.
//!
//! upstream: options.c:1620-1656 and :1718-1727 - a server parses them in the
//! same `parse_arguments()` as the client. They land in `filter_list` ahead of
//! the rules `recv_filter_list()` then appends from the wire
//! (exclude.c:1971-1986). A remote-shell server reads the files they name
//! relative to its cwd; a daemon reads them beneath its module.

use std::ffi::OsString;

use protocol::filters::{FilterRuleWireFormat, RuleType};

use super::{FilterFileSource, FilterInputs, FilterOrderToken, build_filter_rules};
use crate::client::remote::build_wire_format_rules;
use crate::message::Message;
use crate::server::server_argv::{FILTER_OPTIONS, split_at_end_of_options};

/// Builds the rules the filter options in a normalized server argv add.
///
/// `normalize_server_argv` has already joined every filter option to its
/// value and expanded each `-F`, so only `--opt=VALUE` tokens remain. `files`
/// says where the files they name are read from.
///
/// # Errors
///
/// The first malformed rule or unreadable filter file, as upstream reports
/// it before exiting.
pub fn server_argv_filter_rules(
    args: &[OsString],
    from0: bool,
    files: &dyn FilterFileSource,
) -> Result<Vec<FilterRuleWireFormat>, Message> {
    let order = filter_order(args);
    if order.is_empty() {
        return Ok(Vec::new());
    }
    let cvs = filters::CvsSession::default();
    let specs = build_filter_rules(FilterInputs { order, from0 }, &cvs, files)?;
    let rules = build_wire_format_rules(&specs, false).map_err(|error| error.message().clone())?;
    Ok(rules.into_iter().map(side_blind).collect())
}

/// An argv rule as the server applies it: on whichever side it names.
///
/// upstream: check_filter() never consults `FILTRULES_SIDES`. A rule's side
/// only steers what send_rules() puts on the wire (exclude.c:1904-1906) and the
/// add-time drop under `XFLG_ANCHORED2ABS|XFLG_ABS_IF_SLASH` (exclude.c:279-285),
/// and parse_arguments() adds argv rules with neither (options.c:1620-1656). So
/// `P a` (`-r a`) hides `a` from a server sender and `H a` (`-s a`) keeps
/// `a` from a server receiver's deletions.
pub(super) fn side_blind(mut rule: FilterRuleWireFormat) -> FilterRuleWireFormat {
    rule.rule_type = match rule.rule_type {
        RuleType::Protect => RuleType::Exclude,
        RuleType::Risk => RuleType::Include,
        other => other,
    };
    rule.sender_side = false;
    rule.receiver_side = false;
    rule
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
