//! Builds the base clap command with all core argument definitions.
//!
//! Arguments are organized into focused submodules by category,
//! each contributing its group of flags via a builder-pattern extension.

mod core_args;
mod devices;
mod links;
mod network;
mod output;
mod privileges;
mod transfer;

use super::super::{Arg, ArgAction, ClapCommand, OsStringValueParser};

/// Constructs the base `ClapCommand` with all argument definitions.
///
/// Each category of arguments is added by a dedicated submodule function,
/// keeping individual modules focused on a single responsibility.
pub(crate) fn build_base_command(program_name: &'static str) -> ClapCommand {
    let command = ClapCommand::new(program_name)
        .disable_help_flag(true)
        .disable_version_flag(true)
        .arg_required_else_help(false);

    let command = core_args::add_core_args(command);
    let command = output::add_output_args(command);
    let command = network::add_network_args(command);
    let command = transfer::add_transfer_args(command);
    let command = links::add_link_args(command);
    let command = devices::add_device_args(command);
    privileges::add_privilege_args(command)
}
