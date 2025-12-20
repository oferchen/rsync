//! crates/logging/src/lib.rs
//! Logging and verbosity flag system for info and debug output control.
//!
//! This crate provides a thread-local verbosity configuration system that controls
//! which diagnostic messages are emitted. It is intentionally dependency-free to
//! avoid circular dependencies when used by low-level crates.
//!
//! # Usage
//!
//! ```rust,ignore
//! use logging::{VerbosityConfig, info_log, debug_log};
//!
//! // Initialize at session start
//! logging::init(VerbosityConfig::from_verbose_level(2));
//!
//! // Use logging macros anywhere
//! info_log!(Name, 1, "updated: {}", path);
//! debug_log!(Deltasum, 2, "block {} matched", block_num);
//! ```

#![deny(missing_docs)]
#![deny(unsafe_code)]

mod config;
mod levels;
mod macros;
mod thread_local;

pub use config::VerbosityConfig;
pub use levels::{DebugFlag, DebugLevels, InfoFlag, InfoLevels};
pub use thread_local::{
    DiagnosticEvent, apply_debug_flag, apply_info_flag, debug_gte, drain_events, emit_debug,
    emit_info, info_gte, init,
};
