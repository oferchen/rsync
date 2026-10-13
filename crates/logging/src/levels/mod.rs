//! Flag enums and level structures for info and debug verbosity.
//!
//! Each flag corresponds to an entry in upstream rsync's `info_verbosity[]`
//! and `debug_verbosity[]` tables (upstream: options.c:244-259). The flag
//! names match upstream exactly - for example, `InfoFlag::Flist` corresponds
//! to upstream's `INFO_FLIST`, and `DebugFlag::Deltasum` to `DEBUG_DELTASUM`.
//!
//! Deviation from upstream: `DebugFlag` adds four oc-only categories (`Iouring`,
//! `Clone`, `Sockopt`, `Iocp`) with no `DEBUG_*` counterpart.

mod debug;
mod info;

pub use debug::{DebugFlag, DebugLevels};
pub use info::{InfoFlag, InfoLevels};
