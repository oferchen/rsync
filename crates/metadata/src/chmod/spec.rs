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
/// `ModeCOPY_SRC`, `ModeCOPY_DST`, `ModeCOPY_AND`, `ModeOP`, `flags`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Clause {
    /// Bits retained from the existing mode. upstream: `ModeAND`.
    pub(crate) mode_and: u32,
    /// Bits unconditionally set after masking. upstream: `ModeOR`.
    pub(crate) mode_or: u32,
    /// Who-class to copy resolved perms from (`0o100`/`0o010`/`0o001`), or `0`
    /// when the clause carries no copy-from-category letter. upstream:
    /// `ModeCOPY_SRC`.
    pub(crate) copy_src: u32,
    /// Who-classes the copied perms are written into. upstream: `ModeCOPY_DST`.
    pub(crate) copy_dst: u32,
    /// Mask applied to the distributed copy bits (`CHMOD_BITS` for an explicit
    /// who, `~umask` for the implied who). upstream: `ModeCOPY_AND`.
    pub(crate) copy_and: u32,
    /// Whether the clause operator is `-`, so copied bits are cleared instead of
    /// set. upstream: `ModeOP == CHMOD_SUB`.
    pub(crate) is_sub: bool,
    /// `X` conditional-execute flag. upstream: `FLAG_X_KEEP`.
    pub(crate) x_keep: bool,
    /// `D` selector: apply to directories only. upstream: `FLAG_DIRS_ONLY`.
    pub(crate) dirs_only: bool,
    /// `F` selector: apply to non-directories only. upstream: `FLAG_FILES_ONLY`.
    pub(crate) files_only: bool,
}
