mod chown;
mod compression;
mod drive;
mod file_list;
mod flags;
mod module_list;
mod operands;
mod options;

pub(super) use drive::execute;

#[cfg(test)]
use super::arguments::ProgramName;
pub(crate) use chown::parse_chown_argument;
pub(crate) use compression::{
    CompressLevelArg, parse_bandwidth_limit, parse_compress_level, parse_compress_level_argument,
};
#[cfg(test)]
pub(crate) use drive::CONNECT_PROGRAM_DAEMON_ONLY_MESSAGE;
#[cfg(test)]
pub(crate) use file_list::read_file_list_from_reader;
pub(crate) use file_list::{
    load_file_list_operands, resolve_file_list_entries, transfer_requires_remote,
};
pub(crate) use flags::{
    DEBUG_HELP_TEXT, INFO_HELP_TEXT, info_flags_include_progress, parse_debug_flags,
    parse_info_flags,
};
pub(crate) use module_list::render_module_list;
pub(crate) use operands::{extract_operands, parse_bind_address_argument};
#[cfg(test)]
pub(crate) use options::{SizeParseError, pow_u128_for_size};
pub(crate) use options::{
    parse_checksum_seed_argument, parse_human_readable_level, parse_max_delete_argument,
    parse_modify_window_argument, parse_protocol_version_arg, parse_size_limit_argument,
    parse_timeout_argument,
};

#[cfg(test)]
pub(crate) fn render_missing_operands_stdout(program_name: ProgramName) -> String {
    drive::render_missing_operands_stdout(program_name)
}
