//! CLI argument parsing for the rsync frontend.
//!
//! Hosts the [`ParsedArgs`] structure produced by [`parse_args`] together with
//! supporting helpers for environment defaults, short-option cluster expansion,
//! tri-state flag resolution, program-name detection, bandwidth arguments, and
//! `--stop-*` deadline parsing.

mod bandwidth;
mod env;
mod parsed_args;
mod parser;
mod program_name;
mod short_options;
mod stop;

#[cfg(test)]
mod tests;

pub(crate) use bandwidth::BandwidthArgument;
pub(crate) use env::env_protect_args_default;
pub use parsed_args::ParsedArgs; // Changed to pub for test_utils
pub(crate) use parser::ChecksumThreadsSetting;
pub use parser::parse_args; // Changed to pub for test_utils
pub(crate) use program_name::{ProgramName, detect_program_name};
pub(crate) use stop::StopRequest;
