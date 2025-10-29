use super::numbers::{encode_signed_decimal, encode_unsigned_decimal};
use super::source::{
    append_normalized_os_str, canonicalize_or_fallback, compute_workspace_root, normalize_path,
    strip_normalized_workspace_prefix,
};
use super::*;
use crate::{
    message_source, message_source_from, rsync_error, rsync_exit_code, rsync_info, rsync_warning,
    tracked_message_source,
};
use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::{self, IoSlice, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::str::FromStr;

const TESTS_DIR: &str = "crates/core/src/message/tests";

#[track_caller]
fn tracked_source() -> SourceLocation {
    tracked_message_source!()
}

#[track_caller]
fn untracked_source() -> SourceLocation {
    message_source!()
}

#[track_caller]
fn tracked_rsync_error_macro() -> Message {
    rsync_error!(23, "delta-transfer failure")
}

#[track_caller]
fn tracked_rsync_warning_macro() -> Message {
    rsync_warning!("some files vanished")
}

#[track_caller]
fn tracked_rsync_info_macro() -> Message {
    rsync_info!("negotiation complete")
}

#[track_caller]
fn tracked_rsync_exit_code_macro() -> Message {
    rsync_exit_code!(23).expect("exit code 23 is defined")
}

include!("part1.rs");
include!("part2.rs");
include!("part3.rs");
include!("part4.rs");
include!("part5.rs");
include!("part6.rs");
