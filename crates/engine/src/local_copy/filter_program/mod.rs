//! Filter program and per-directory merge rules.
//!
//! A [`FilterProgram`] is an ordered list of filter instructions, per-directory
//! merge directives ([`DirMergeRule`]), and exclude-if-present markers
//! ([`ExcludeIfPresentRule`]) that together determine which source entries
//! participate in a transfer.

mod options;
mod program;
mod rules;
mod segments;

pub(crate) use options::DirMergeParser;
pub use options::{DirMergeEnforcedKind, DirMergeOptions};

#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use program::{
    CONNECTION_TIMEOUT_EXIT_CODE, INVALID_OPERAND_EXIT_CODE, MAX_DELETE_EXIT_CODE,
    MISSING_OPERANDS_EXIT_CODE, TIMEOUT_EXIT_CODE,
};
pub use program::{FilterProgram, FilterProgramEntry, FilterProgramError};

pub(crate) use rules::directory_has_marker;
pub use rules::{DirMergeRule, ExcludeIfPresentRule};

pub(crate) use segments::{
    ExcludeIfPresentLayers, ExcludeIfPresentStack, FilterContext, FilterSegment,
    FilterSegmentLayers, FilterSegmentStack,
};

#[cfg(test)]
pub(crate) use segments::FilterOutcome;
