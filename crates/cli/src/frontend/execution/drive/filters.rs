#![deny(unsafe_code)]

use core::client::ClientConfigBuilder;
pub(crate) use core::client::filter_args::{FilterInputs, build_filter_rules};
use logging_sink::MessageSink;

use super::messages::fail_with_message;
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
    let filter_rules = build_filter_rules(inputs, &cvs, &ProcessFilterFiles)
        .map_err(|message| fail_with_message(message, stderr))?;
    let builder = if filter_rules.is_empty() {
        builder
    } else {
        builder.extend_filter_rules(filter_rules)
    };
    Ok(builder.cvs_session(cvs))
}
