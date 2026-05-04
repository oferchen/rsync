//! Flag enums and level structures for info and debug verbosity.
//!
//! Each flag corresponds to an entry in upstream rsync's `info_verbosity[]`
//! and `debug_verbosity[]` tables (upstream: options.c:228-243). The flag
//! names match upstream exactly - for example, `InfoFlag::Flist` corresponds
//! to upstream's `INFO_FLIST`, and `DebugFlag::Deltasum` to `DEBUG_DELTASUM`.

mod debug;
mod info;

pub use debug::{DebugFlag, DebugLevels};
pub use info::{InfoFlag, InfoLevels};
