//! crates/logging/src/verbosity.rs
//! Verbosity configuration for info and debug flags.

mod levels;
mod config;
mod thread_local;
mod macros;

pub use levels::{InfoFlag, DebugFlag, InfoLevels, DebugLevels};
pub use config::VerbosityConfig;
pub use thread_local::{init, info_gte, debug_gte, emit_info, emit_debug, drain_events, apply_info_flag, apply_debug_flag, DiagnosticEvent};
