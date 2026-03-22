//! Logging and verbosity flag system for info and debug output control.
//!
//! This crate provides a thread-local verbosity configuration system that controls
//! which diagnostic messages are emitted. It mirrors upstream rsync's two-tier
//! flag system - info flags for user-visible output and debug flags for developer
//! diagnostics - as defined in `options.c:set_output_verbosity()`.
//!
//! # Architecture
//!
//! Upstream rsync uses two parallel arrays of per-category verbosity levels:
//! `info_verbosity[]` and `debug_verbosity[]` (upstream: options.c:228-243).
//! The `-v` flag increments a global verbose counter, and
//! `set_output_verbosity()` (upstream: options.c:513) walks both tables
//! cumulatively from level 0 through the requested level, setting individual
//! flags. Fine-grained overrides are available via `--info=FLAGS` and
//! `--debug=FLAGS` (upstream: options.c:parse_output_words()).
//!
//! This crate replicates that design:
//!
//! - [`VerbosityConfig`] holds [`InfoLevels`] and [`DebugLevels`], one `u8` per
//!   flag category.
//! - [`VerbosityConfig::from_verbose_level`] applies the same cumulative mapping
//!   as upstream's `set_output_verbosity()`.
//! - [`init`] installs the config into thread-local storage so that the
//!   [`info_log!`] and [`debug_log!`] macros can check levels without passing
//!   state.
//! - Events are collected into a thread-local buffer and drained via
//!   [`drain_events`] for the caller to route to stderr, the multiplex stream,
//!   or log files - mirroring upstream's `rwrite()` dispatch
//!   (upstream: log.c:rwrite()).
//!
//! # Verbosity-to-flag mapping
//!
//! | `-v` level | Info flags enabled | Debug flags enabled |
//! |------------|-------------------|-------------------|
//! | 0 | `NONREG` | (none) |
//! | 1 (`-v`) | `COPY,DEL,FLIST,MISC,NAME,STATS,SYMSAFE` | (none) |
//! | 2 (`-vv`) | `BACKUP,MISC2,MOUNT,NAME2,REMOVE,SKIP` | `BIND,CMD,CONNECT,DEL,DELTASUM,DUP,FILTER,FLIST,ICONV` |
//! | 3 (`-vvv`) | (same as 2) | `ACL,BACKUP,CONNECT2,DELTASUM2,DEL2,EXIT,FILTER2,FLIST2,FUZZY,GENR,OWN,RECV,SEND,TIME` |
//! | 4+ | (same as 2) | `CMD2,DELTASUM3-4,DEL3,EXIT2,FLIST3-4,FUZZY2,HASH,HLINK,ICONV2,OWN2,PROTO,TIME2,CHDIR` |
//!
//! # Error formatting
//!
//! The [`rsync_error_fmt!`] and [`rsync_warning_fmt!`] macros produce strings
//! matching upstream's diagnostic format (upstream: log.c:rwrite(),
//! errcode.h):
//!
//! ```text
//! rsync error: <text> (code N) at <file>:<line> [<role>=<version>]
//! ```
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
//!
//! # Optional features
//!
//! - **`serde`** - Adds `Serialize`/`Deserialize` to all config types.
//! - **`tracing`** - Bridges the `tracing` crate to rsync's verbosity system
//!   via `RsyncLayer`, mapping tracing targets like `rsync::copy` to the
//!   corresponding info/debug flags.

#![deny(missing_docs)]
#![deny(unsafe_code)]

mod config;
pub mod error_format;
mod levels;
mod macros;
mod phase_timer;
mod thread_local;

#[cfg(feature = "tracing")]
mod tracing_bridge;

#[cfg(feature = "tracing")]
mod tracing_macros;

pub use config::VerbosityConfig;
pub use error_format::{format_rsync_error, format_rsync_warning, strip_repo_prefix};
pub use levels::{DebugFlag, DebugLevels, InfoFlag, InfoLevels};
pub use phase_timer::PhaseTimer;
pub use thread_local::{
    DiagnosticEvent, apply_debug_flag, apply_info_flag, debug_gte, drain_events, emit_debug,
    emit_info, info_gte, init,
};

#[cfg(feature = "tracing")]
pub use tracing_bridge::{RsyncLayer, init_tracing, init_tracing_with_filter};
