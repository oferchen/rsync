//! `--debug=CHDIR` producer emissions for current-directory changes.
//!
//! Hosts the trace helper that mirrors upstream rsync 3.4.4's single
//! `DEBUG_GTE(CHDIR, 1)` emission from `util1.c::change_dir`. Upstream
//! routes every successful `chdir()` syscall through `change_dir`, so the
//! single helper here is the wire-equivalent of `[<who>] change_dir(<cwd>)`.
//!
//! # Upstream Reference
//!
//! - `util1.c:1113-1172` - `change_dir(const char *dir, int set_path_only)`.
//! - `util1.c:1168-1169` - the sole `DEBUG_GTE(CHDIR, 1)` site emitting
//!   `"[%s] change_dir(%s)\n"` after a successful `chdir()` syscall.
//! - `options.c:293` - `DEBUG_WORD(CHDIR, W_CLI|W_SRV, ...)` flag table
//!   entry, capping useful emissions at level 1.

pub mod trace;

pub use trace::{ChdirRole, trace_change_dir};
