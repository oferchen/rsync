//! A daemon module's own filter directives.
//!
//! upstream: clientserver.c:929-954 rsync_module() builds
//! `daemon_filter_list` from five parameters, in this order, each through the
//! same parse_filter_str()/parse_filter_file() as every other rule, with
//! `XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3`.

use std::ffi::OsStr;
use std::path::Path;

use protocol::filters::FilterRuleWireFormat;

use super::FilterFileSource;
use super::filter_str::{ParseContext, Template, Xflags, parse_filter_str};
use super::server_argv::side_blind;
use super::sources::append_filter_rules_from_files;
use crate::client::remote::build_wire_format_rules;
use crate::client::{FilterRuleKind, FilterRuleSpec};
use crate::message::Message;

/// A module's filter parameters, as rsyncd.conf gives them.
#[derive(Clone, Copy, Debug, Default)]
pub struct ModuleFilterDirectives<'a> {
    /// `filter`: full rule syntax, one rule per word.
    pub filter: &'a [String],
    /// `include from`: one include pattern per line.
    pub include_from: Option<&'a Path>,
    /// `include`: one include pattern per word.
    pub include: &'a [String],
    /// `exclude from`: one exclude pattern per line.
    pub exclude_from: Option<&'a Path>,
    /// `exclude`: one exclude pattern per word.
    pub exclude: &'a [String],
}

/// The flags every module directive is parsed with (clientserver.c:933-952).
const MODULE_XFLAGS: Xflags = Xflags {
    old_prefixes: false,
    abs_if_slash: true,
    dir2wild3: true,
};

/// Builds a module's `daemon_filter_list`.
///
/// upstream: clientserver.c:933-952. `files` opens what the directives name:
/// its `base_dir` is upstream's `dirbuf`, the module root, and its `open` sees
/// the list built so far, which is what gates the open (exclude.c:1639-1664).
///
/// # Errors
///
/// The first malformed rule or unreadable file, as upstream reports it before
/// refusing the module.
pub fn module_filter_rules(
    directives: &ModuleFilterDirectives<'_>,
    files: &dyn FilterFileSource,
) -> Result<Vec<FilterRuleWireFormat>, Message> {
    let mut list = Vec::new();
    let base_dir = files.base_dir();
    let mut merge_stack = Vec::new();
    let cvs = filters::CvsSession::default();
    let mut ctx = ParseContext {
        base_dir: &base_dir,
        merge_stack: &mut merge_stack,
        cvs: &cvs,
        files,
    };
    let words = |kind| Template {
        kind,
        word_split: true,
    };
    let old_prefixes = Xflags {
        old_prefixes: true,
        ..MODULE_XFLAGS
    };

    // upstream: clientserver.c:933-935 - rule_template(FILTRULE_WORD_SPLIT).
    for text in directives.filter {
        let template = words(FilterRuleKind::Exclude);
        parse_filter_str(
            &mut list,
            OsStr::new(text),
            template,
            MODULE_XFLAGS,
            &mut ctx,
        )?;
    }
    // upstream: clientserver.c:937-939 - rule_template(FILTRULE_INCLUDE), plus
    // XFLG_OLD_PREFIXES | XFLG_FATAL_ERRORS.
    if let Some(path) = directives.include_from {
        let names = [path.as_os_str().to_os_string()];
        append_filter_rules_from_files(
            &mut list,
            &names,
            FilterRuleKind::Include,
            false,
            files,
            MODULE_XFLAGS,
        )?;
    }
    // upstream: clientserver.c:941-944 - FILTRULE_INCLUDE | FILTRULE_WORD_SPLIT.
    for text in directives.include {
        let template = words(FilterRuleKind::Include);
        parse_filter_str(
            &mut list,
            OsStr::new(text),
            template,
            old_prefixes,
            &mut ctx,
        )?;
    }
    // upstream: clientserver.c:946-948 - rule_template(0).
    if let Some(path) = directives.exclude_from {
        let names = [path.as_os_str().to_os_string()];
        append_filter_rules_from_files(
            &mut list,
            &names,
            FilterRuleKind::Exclude,
            false,
            files,
            MODULE_XFLAGS,
        )?;
    }
    // upstream: clientserver.c:950-952 - FILTRULE_WORD_SPLIT.
    for text in directives.exclude {
        let template = words(FilterRuleKind::Exclude);
        parse_filter_str(
            &mut list,
            OsStr::new(text),
            template,
            old_prefixes,
            &mut ctx,
        )?;
    }
    daemon_list_rules(&list)
}

/// A daemon list in the form the daemon matches it.
///
/// The list is never sent, and check_filter() never consults a rule's sides
/// (see `side_blind`), so every rule applies on both sides.
///
/// # Errors
///
/// A rule the wire form cannot carry.
pub fn daemon_list_rules(list: &[FilterRuleSpec]) -> Result<Vec<FilterRuleWireFormat>, Message> {
    let rules = build_wire_format_rules(list, false).map_err(|error| error.message().clone())?;
    Ok(rules.into_iter().map(side_blind).collect())
}
