mod sections;

pub(super) use clap::{Arg, ArgAction, Command as ClapCommand, builder::OsStringValueParser};

pub(crate) fn clap_command(program_name: &'static str) -> ClapCommand {
    let command = sections::build_base_command(program_name);
    let command = sections::add_transfer_behavior_options(command);
    sections::add_connection_and_logging_options(command)
}
