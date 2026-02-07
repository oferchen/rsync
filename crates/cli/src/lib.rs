#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Thin command-line frontend that orchestrates argument parsing and execution
//! for the single `oc-rsync` binary.
//!
//! This crate exposes [`run`] as the primary entry point so all binaries can
//! share the same parsing and dispatch logic. The actual implementation lives in
//! the internal `frontend` module, which keeps the majority of the code
//! organised away from the crate root. Only the surface API is re-exported here
//! to keep the entry point concise while satisfying the repository's file-size
//! hygiene policy.

mod frontend;
mod platform;

pub use frontend::dry_run::{
    DryRunAction, DryRunFormatter, DryRunSummary, format_number_with_commas,
};
pub use frontend::info_output;
pub use frontend::itemize::{FileType, ItemizeChange, UpdateType, format_itemize};
pub use frontend::progress_format;
pub use frontend::stats_format;
pub use frontend::{exit_code_from, run};

/// Test utilities exposed for integration tests.
///
/// This module provides access to internal parsing functions and types
/// needed for comprehensive argument validation tests.
///
/// **Warning**: This is not part of the public API and may change without notice.
#[doc(hidden)]
pub mod test_utils {
    pub use crate::frontend::arguments::{ParsedArgs, parse_args};
    pub use crate::frontend::progress::{NameOutputLevel, ProgressSetting};
}

#[allow(unused_imports)]
pub(crate) use frontend::password;
#[allow(unused_imports)]
pub(crate) use frontend::{
    LIST_TIMESTAMP_FORMAT, OutFormat, OutFormatContext, describe_event_kind, emit_out_format,
    format_list_permissions, parse_out_format,
};

#[cfg(test)]
mod tests {
    use super::exit_code_from;
    use std::process::ExitCode;

    #[test]
    fn exit_code_from_clamps_negative_values() {
        assert_eq!(exit_code_from(-5), ExitCode::from(0));
    }

    #[test]
    fn exit_code_from_clamps_large_values() {
        assert_eq!(exit_code_from(1_000), ExitCode::from(u8::MAX));
    }

    #[test]
    fn exit_code_from_preserves_valid_values() {
        assert_eq!(exit_code_from(42), ExitCode::from(42));
    }
}
