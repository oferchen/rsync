#![deny(unsafe_code)]

use core::client::ClientConfigBuilder;
use core::client::filter_args::write_refusal;
pub(crate) use core::client::filter_args::{FilterInputs, build_filter_rules};
use core::message::Role;
use logging_sink::MessageSink;

use crate::frontend::filter_rules::ProcessFilterFiles;

/// Applies CLI-provided filter rules to the [`ClientConfigBuilder`].
///
/// See [`build_filter_rules`] for the order the rules take.
pub(crate) fn apply_filters<Err>(
    builder: ClientConfigBuilder,
    inputs: FilterInputs,
    stderr: &mut MessageSink<Err>,
) -> Result<ClientConfigBuilder, i32>
where
    Err: std::io::Write,
{
    let cvs = filters::CvsSession::default();
    // upstream: options.c parses these in the client before any fork, where
    // who_am_i() is "client" (rsync.c:989-990).
    let filter_rules = build_filter_rules(inputs, &cvs, &ProcessFilterFiles { who: Role::Client })
        .map_err(|refusal| {
            let brand = stderr.brand();
            let style = stderr.escape_style();
            write_refusal(stderr.writer_mut(), brand, Role::Client, &refusal, style)
                .unwrap_or_else(|_| refusal.code().unwrap_or(1))
        })?;
    let builder = if filter_rules.is_empty() {
        builder
    } else {
        builder.extend_filter_rules(filter_rules)
    };
    Ok(builder.cvs_session(cvs))
}
