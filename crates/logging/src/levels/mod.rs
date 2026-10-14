//! Flag enums and level structures for info and debug verbosity.
//!
//! Each flag corresponds to an entry in upstream rsync's `info_verbosity[]`
//! and `debug_verbosity[]` tables (upstream: options.c:244-259). The flag
//! names match upstream exactly - for example, `InfoFlag::Flist` corresponds
//! to upstream's `INFO_FLIST`, and `DebugFlag::Deltasum` to `DEBUG_DELTASUM`.
//!
//! Deviation from upstream: `DebugFlag` adds six oc-only categories (`Iouring`,
//! `Clone`, `Sockopt`, `Iocp`, `Seccomp`, `Landlock`) with no `DEBUG_*`
//! counterpart. `Seccomp` and `Landlock` are the first oc-only categories a
//! `-v` count implies: level 1 (the layer's decision) at `-vvv`, alongside
//! RECV (upstream: options.c:248), and level 2 (its detail) at `-vvvv`. `-v`
//! and `-vv` are untouched, so their output stays byte-identical to
//! upstream. The `sandbox` debug word is a parser alias that sets both.

mod debug;
mod info;

pub use debug::{DebugFlag, DebugLevels};
pub use info::{InfoFlag, InfoLevels};
