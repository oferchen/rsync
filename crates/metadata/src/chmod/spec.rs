//! Crate-internal representation of parsed `--chmod` clauses.
//!
//! Mirrors upstream rsync's `chmod.c:struct chmod_mode_struct`: each clause is
//! reduced to an AND mask and an OR mask plus the three behavioural flags
//! (`FLAG_X_KEEP`, `FLAG_DIRS_ONLY`, `FLAG_FILES_ONLY`). The evaluator applies
//! `mode = (mode & ModeAND) | ModeOR` clause by clause, exactly as
//! `chmod.c:tweak_mode()` does. None of these types are exposed publicly.

/// Permission bits touched by a chmod clause. upstream: `rsync.h` `CHMOD_BITS`
/// (setuid, setgid, sticky, plus the nine rwx bits).
pub(crate) const CHMOD_BITS: u32 = 0o7777;

/// One parsed `--chmod` clause reduced to an AND/OR transform.
///
/// upstream: chmod.c `struct chmod_mode_struct` fields `ModeAND`, `ModeOR`,
/// `flags`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Clause {
    /// Bits retained from the existing mode. upstream: `ModeAND`.
    pub(crate) mode_and: u32,
    /// Bits unconditionally set after masking. upstream: `ModeOR`.
    pub(crate) mode_or: u32,
    /// `X` conditional-execute flag. upstream: `FLAG_X_KEEP`.
    pub(crate) x_keep: bool,
    /// `D` selector: apply to directories only. upstream: `FLAG_DIRS_ONLY`.
    pub(crate) dirs_only: bool,
    /// `F` selector: apply to non-directories only. upstream: `FLAG_FILES_ONLY`.
    pub(crate) files_only: bool,
}
