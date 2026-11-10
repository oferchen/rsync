//! Unit tests for the filter-argument parser.
//!
//! Each test reads files through [`TestFiles`]: the cwd, with a standard input
//! the test fills.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use filters::CvsSession;

use super::directive::{FilterDirective, MergeDirective, merge_directive_options};
use super::parsing::{parse_filter_directive, parse_merge_modifiers_from_argument};
use super::{TestFiles, set_filter_stdin_input};
use crate::client::{DirMergeEnforcedKind, DirMergeOptions, FilterRuleKind, FilterRuleSpec};
use crate::message::Message;

mod apply;
mod load;
mod merge_directive;
mod parse_filter;
mod process;

fn load_filter_file_patterns(path: &Path) -> Result<Vec<String>, Message> {
    super::sources::load_filter_file_patterns(path, false, false, &TestFiles, &[])
}

fn parse_merge_modifiers(
    modifiers: &str,
    directive: &str,
) -> Result<(DirMergeOptions, bool), Message> {
    parse_merge_modifiers_from_argument(modifiers, directive)
}

fn apply_merge_directive(
    directive: MergeDirective,
    base_dir: &Path,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut Vec<PathBuf>,
    source: filters::RuleSource<'_>,
    cvs: &CvsSession,
) -> Result<(), Message> {
    super::merge::apply_merge_directive(
        directive,
        base_dir,
        destination,
        visited,
        source,
        cvs,
        &TestFiles,
    )
}

fn process_merge_directive(
    directive: &str,
    options: &DirMergeOptions,
    base_dir: &Path,
    source: filters::RuleSource<'_>,
    destination: &mut Vec<FilterRuleSpec>,
    visited: &mut Vec<PathBuf>,
    cvs: &CvsSession,
) -> Result<(), Message> {
    super::merge::process_merge_directive(
        directive,
        options,
        base_dir,
        source,
        destination,
        visited,
        cvs,
        &TestFiles,
    )
}
