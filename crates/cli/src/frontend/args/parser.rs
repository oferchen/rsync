use std::env;
use std::ffi::OsString;
use std::path::PathBuf;

use rsync_core::client::{AddressMode, DeleteMode, HumanReadableMode, StrongChecksumChoice};

use super::super::command_builder::clap_command;
use super::super::filter_rules::{collect_filter_arguments, locate_filter_arguments};
use super::super::program::detect_program_name;
use super::super::progress::{NameOutputLevel, ProgressSetting};
use super::super::{
    parse_checksum_seed_argument, parse_compress_level_argument, parse_human_readable_level,
};
use super::types::{BandwidthArgument, ParsedArgs};

include!("parser_body.rs");
