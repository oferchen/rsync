mod sections;

pub(super) use clap::{Arg, ArgAction, Command as ClapCommand, builder::OsStringValueParser};

pub(super) fn clap_command(program_name: &'static str) -> ClapCommand {
    let command = sections::section_01(program_name);
    let command = sections::section_02(command);
    let command = sections::section_03(command);
    command
}
