mod options;
mod program;
mod rules;
mod segments;

pub(crate) use options::DirMergeParser;
pub use options::{DirMergeEnforcedKind, DirMergeOptions};

pub use program::{FilterProgram, FilterProgramEntry, FilterProgramError};
pub(crate) use program::{
    CONNECTION_TIMEOUT_EXIT_CODE, INVALID_OPERAND_EXIT_CODE, MAX_DELETE_EXIT_CODE,
    MISSING_OPERANDS_EXIT_CODE, TIMEOUT_EXIT_CODE,
};

pub(crate) use rules::directory_has_marker;
pub use rules::{DirMergeRule, ExcludeIfPresentRule};

pub(crate) use segments::{
    ExcludeIfPresentLayers, ExcludeIfPresentStack, FilterContext, FilterSegment,
    FilterSegmentLayers, FilterSegmentStack,
};

#[cfg(test)]
pub(crate) use segments::FilterOutcome;
