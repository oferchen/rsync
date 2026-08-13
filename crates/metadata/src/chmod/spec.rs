//! Crate-internal representation of parsed `--chmod` clauses.
//!
//! Mirrors upstream rsync's `chmod.c:struct chmod_mode_struct`: each clause is
//! reduced to an AND mask and an OR mask, a permission-copy triple, the
//! originating operator, and the three behavioural flags (`FLAG_X_KEEP`,
//! `FLAG_DIRS_ONLY`, `FLAG_FILES_ONLY`). The evaluator applies
//! `mode = (mode & ModeAND) | ModeOR` and then folds in the copied bits clause
//! by clause, exactly as `chmod.c:tweak_mode()` does. None of these types are
//! exposed publicly.

/// Permission bits touched by a chmod clause. upstream: `rsync.h` `CHMOD_BITS`
/// (setuid, setgid, sticky, plus the nine rwx bits).
pub(crate) const CHMOD_BITS: u32 = 0o7777;

/// Operator a clause was built from. upstream: chmod.c `CHMOD_ADD`,
/// `CHMOD_SUB`, `CHMOD_EQ`, `CHMOD_SET` stored as `ModeOP`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Op {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `=`
    Eq,
    /// A bare octal mode.
    Set,
}

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
    /// Who-class the copied permissions are read *from*, as a `0o100`/`0o010`/
    /// `0o001` selector; zero when the clause copies nothing. upstream:
    /// `ModeCOPY_SRC`.
    pub(crate) copy_src: u32,
    /// Who-classes the copied permissions are written *to*, as the same
    /// selector the clause's `bits` were multiplied by. upstream:
    /// `ModeCOPY_DST`.
    pub(crate) copy_dst: u32,
    /// Mask applied to the spread copied bits: `CHMOD_BITS` when the who-class
    /// was written out, `!umask` when it was implied. upstream: `ModeCOPY_AND`.
    pub(crate) copy_and: u32,
    /// Operator this clause came from; only `Op::Sub` inverts the copy fold.
    /// upstream: `ModeOP`.
    pub(crate) op: Op,
    /// `X` conditional-execute flag. upstream: `FLAG_X_KEEP`.
    pub(crate) x_keep: bool,
    /// `D` selector: apply to directories only. upstream: `FLAG_DIRS_ONLY`.
    pub(crate) dirs_only: bool,
    /// `F` selector: apply to non-directories only. upstream: `FLAG_FILES_ONLY`.
    pub(crate) files_only: bool,
}
