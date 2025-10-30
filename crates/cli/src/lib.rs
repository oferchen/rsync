#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! Thin command-line frontend that orchestrates argument parsing and execution
//! for the `oc-rsync` and `oc-rsyncd` binaries.
//!
//! This crate exposes [`run`] as the primary entry point so both binaries can
//! share the same parsing and dispatch logic. The actual implementation lives in
//! the internal `frontend` module, which keeps the majority of the code
//! organised away from the crate root. Only the surface API is re-exported here
//! to keep the entry point concise while satisfying the repository's file-size
//! hygiene policy.

mod frontend;
mod platform;

pub use frontend::{exit_code_from, run};

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
