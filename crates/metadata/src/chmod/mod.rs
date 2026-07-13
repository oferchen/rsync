#![allow(clippy::cast_possible_truncation)]

//! Parser and evaluator for receiver-side `--chmod` modifiers.
//!
//! The upstream rsync CLI allows multiple `--chmod=SPEC` occurrences where each
//! specification may contain comma-separated numeric or symbolic clauses. This
//! module mirrors upstream rsync's `chmod.c:parse_chmod()` grammar exactly,
//! reducing every clause to an AND/OR mask pair (`ModeAND`/`ModeOR`) plus the
//! `D`/`F` selectors, then applying them through `chmod.c:tweak_mode()` order:
//! conditional execute bits (`X`), the set-id/sticky bits driven by the who
//! letters, and the umask masking applied to an implied who-class. A category
//! letter (`u`/`g`/`o`) in the permission half is rejected exactly as upstream
//! does - rsync has no chmod(1)-style copy-from-category form. The
//! [`ChmodModifiers`] type wraps the parsed clauses and exposes
//! [`ChmodModifiers::apply`] so higher layers can evaluate modifiers after the
//! standard metadata preservation step.

mod apply;
mod parse;
mod spec;

use thiserror::Error;

#[cfg(unix)]
use apply::apply_clauses;
use parse::parse_spec;
use spec::Clause;

/// Error produced when parsing a `--chmod` specification fails.
#[derive(Debug, Eq, PartialEq, Error)]
#[error("{message}")]
pub struct ChmodError {
    message: String,
}

impl ChmodError {
    pub(crate) fn new(text: impl Into<String>) -> Self {
        Self {
            message: text.into(),
        }
    }
}

/// Parsed representation of one or more `--chmod` directives.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ChmodModifiers {
    clauses: Vec<Clause>,
}

impl ChmodModifiers {
    /// Parses a comma-separated chmod specification.
    pub fn parse(spec: &str) -> Result<Self, ChmodError> {
        Ok(Self {
            clauses: parse_spec(spec)?,
        })
    }

    /// Appends clauses from another [`ChmodModifiers`] value.
    pub fn extend(&mut self, other: ChmodModifiers) {
        self.clauses.extend(other.clauses);
    }

    /// Applies the modifiers to the provided mode, returning the updated value.
    #[cfg(unix)]
    #[must_use]
    pub fn apply(&self, mode: u32, file_type: std::fs::FileType) -> u32 {
        apply_clauses(&self.clauses, mode, file_type)
    }

    /// Applies the modifiers on non-Unix platforms.
    #[cfg(not(unix))]
    #[must_use]
    pub fn apply(&self, mode: u32, _file_type: std::fs::FileType) -> u32 {
        let _ = mode;
        mode
    }

    /// Returns `true` when no clauses are present.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

/// Owner `rwx` bits (`S_IRWXU`).
const S_IRWXU: u32 = 0o700;
/// Owner execute bit (`S_IXUSR`).
const S_IXUSR: u32 = 0o100;
/// Owner write bit (`S_IWUSR`).
const S_IWUSR: u32 = 0o200;

/// Final on-disk permission bits a directory carries after upstream rsync's
/// during-transfer permission dance, given the `--chmod`-tweaked `mode`.
///
/// upstream: generator.c:1512-1520 raises every directory to owner-`rwx` while
/// its contents are written (`do_chmod_at(fname, file->mode | S_IRWXU)`), then
/// generator.c:2107-2145 `touch_up_dirs()` restores the tweaked mode ONLY when
/// the owner would otherwise lack write
/// (`fix_dir_perms = !am_root && !(file->mode & S_IWUSR)`). The net effect a
/// synchronous local copy must reproduce: a tweak that leaves an owner-writable
/// but not fully owner-`rwx` directory keeps the transient owner bits (e.g.
/// `--chmod=ug=rw` 0o665 -> 0o765), while an owner-non-writable one is left at
/// the strict tweaked mode (e.g. `--chmod=u=r` 0o455 -> 0o455).
///
/// `running_as_root` mirrors `am_root`: a privileged transfer skips the dance
/// entirely because root traverses any directory regardless of its mode.
#[must_use]
pub fn directory_transfer_mode(mode: u32, running_as_root: bool) -> u32 {
    let perm = mode & 0o7777;
    // upstream gates the fixup on `!am_root && dir_tweaking`; root needs no
    // temporary owner bits to write into a directory.
    if running_as_root {
        return perm;
    }
    // The fixup only fires when the owner is not already `rwx`.
    if perm & S_IRWXU == S_IRWXU {
        return perm;
    }
    // touch_up_dirs restores the strict tweaked mode when the owner lacks
    // write; otherwise the transient owner-`rwx` bits persist on disk.
    if perm & S_IWUSR == 0 {
        perm
    } else {
        perm | S_IRWXU
    }
}

/// Whether a *transfer-root* directory self-locks under the tweaked `mode`.
///
/// The transfer root is addressed as `dst/.`, so upstream's during-transfer
/// fixup `do_chmod_at("dst/.", mode | S_IRWXU)` must resolve `.` *inside* `dst`,
/// which needs owner-execute on `dst`. When the tweaked mode strips owner
/// execute the chmod fails with `EACCES` (generator.c:1514 "failed to modify
/// permissions on %s") and the generator can no longer stat or create the
/// directory's contents, so nothing under it transfers and rsync exits 23.
/// Non-root directories are addressed by name and never take this path.
#[must_use]
pub fn transfer_root_self_locks(mode: u32, running_as_root: bool) -> bool {
    !running_as_root && (mode & S_IXUSR == 0)
}

#[cfg(test)]
mod tests;
