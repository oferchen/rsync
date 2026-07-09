//! Evaluator that applies parsed [`Clause`] modifiers to a mode value.
//!
//! Faithful port of upstream rsync's `chmod.c:tweak_mode()`: each clause is a
//! `mode = (mode & ModeAND) | ModeOR` transform, gated by the `D`/`F` selector
//! and the `X` conditional-execute flag. Non-permission bits (the file-type
//! bits above `CHMOD_BITS`) are preserved unchanged.

use super::spec::Clause;
// CHMOD_BITS is only referenced by the unix-only mode-tweaking path; importing
// it unconditionally is an unused import on Windows.
#[cfg(unix)]
use super::spec::CHMOD_BITS;

/// Applies the clause list to `mode`, mirroring `chmod.c:tweak_mode()`.
#[cfg(unix)]
pub(crate) fn apply_clauses(clauses: &[Clause], mode: u32, file_type: std::fs::FileType) -> u32 {
    tweak_mode(clauses, mode, file_type.is_dir())
}

#[cfg(not(unix))]
#[allow(dead_code)] // REASON: used on unix; stub on other platforms
pub(crate) fn apply_clauses(_clauses: &[Clause], mode: u32, _file_type: std::fs::FileType) -> u32 {
    mode
}

/// Resolves the copy-from-category bits for a clause against the live mode.
///
/// upstream: chmod.c `mode_copy_bits()` - extracts the three rwx bits of each
/// selected source class, then distributes them across the destination
/// classes, finally masking with the clause's copy AND (`CHMOD_BITS` or
/// `~umask`).
#[cfg(unix)]
fn mode_copy_bits(mode: u32, copy_src: u32, copy_dst: u32, copy_and: u32) -> u32 {
    let mut copy_bits = 0;
    if copy_src & 0o100 != 0 {
        copy_bits |= (mode >> 6) & 7;
    }
    if copy_src & 0o010 != 0 {
        copy_bits |= (mode >> 3) & 7;
    }
    if copy_src & 0o001 != 0 {
        copy_bits |= mode & 7;
    }
    (copy_dst * copy_bits) & copy_and
}

/// Core of `chmod.c:tweak_mode()`.
///
/// `is_x` is sampled once from the original executable bits and `non_perm`
/// holds the file-type bits, both restored per upstream. upstream:
/// chmod.c:218-236.
#[cfg(unix)]
fn tweak_mode(clauses: &[Clause], orig: u32, is_dir: bool) -> u32 {
    let is_x = orig & 0o111;
    let non_perm = orig & !CHMOD_BITS;
    let mut mode = orig & CHMOD_BITS;

    for clause in clauses {
        // upstream: chmod.c:224-227 - honour the D/F selector.
        if clause.dirs_only && !is_dir {
            continue;
        }
        if clause.files_only && is_dir {
            continue;
        }

        // upstream: chmod.c - copy bits are resolved against the pre-AND mode.
        let copy_bits = mode_copy_bits(mode, clause.copy_src, clause.copy_dst, clause.copy_and);

        mode &= clause.mode_and;

        // upstream: chmod.c:229-232 - a conditional `X` only sets the execute
        // bits when the file was already executable or is a directory.
        if clause.x_keep && is_x == 0 && !is_dir {
            mode |= clause.mode_or & !0o111;
        } else {
            mode |= clause.mode_or;
        }

        // upstream: chmod.c - a `-` copy clause clears the copied bits; every
        // other operator sets them. Non-copy clauses have `copy_bits == 0`,
        // leaving both branches a no-op.
        if clause.is_sub {
            mode &= CHMOD_BITS - copy_bits;
        } else {
            mode |= copy_bits;
        }
    }

    mode | non_perm
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::parse::parse_with_umask;
    use super::*;

    const UMASK: u32 = 0o022;

    fn apply(spec: &str, mode: u32, is_dir: bool) -> u32 {
        let clauses = parse_with_umask(spec, UMASK).expect("parses");
        tweak_mode(&clauses, mode, is_dir) & CHMOD_BITS
    }

    #[test]
    fn octal_sets_exact_mode() {
        assert_eq!(apply("750", 0o644, false), 0o750);
        assert_eq!(apply("0644", 0o777, false), 0o644);
    }

    #[test]
    fn directory_and_file_selectors_route_by_type() {
        // D755,F644: dir -> 755, file -> 644.
        assert_eq!(apply("D755,F644", 0o600, true), 0o755);
        assert_eq!(apply("D755,F644", 0o600, false), 0o644);
    }

    #[test]
    fn add_and_remove_are_relative() {
        assert_eq!(apply("u+x", 0o644, false), 0o744);
        assert_eq!(apply("g-w,o-rwx", 0o666, false), 0o640);
    }

    #[test]
    fn assign_resets_class_but_keeps_setid() {
        // upstream: `u=rx` on 04755 keeps setuid -> 04555.
        assert_eq!(apply("u=rx", 0o4755, false), 0o4555);
        assert_eq!(apply("a=rx", 0o4755, false), 0o4555);
    }

    #[test]
    fn setid_defaults_to_setuid_without_ug_who() {
        // upstream: o+s / a+s / +s all set setuid only.
        assert_eq!(apply("o+s", 0o644, false), 0o4644);
        assert_eq!(apply("a+s", 0o644, false), 0o4644);
        assert_eq!(apply("+s", 0o644, false), 0o4644);
        assert_eq!(apply("g+s", 0o644, false), 0o2644);
    }

    #[test]
    fn sticky_applies_for_any_who() {
        assert_eq!(apply("g+t", 0o644, false), 0o1644);
        assert_eq!(apply("+t", 0o644, true), 0o1644);
    }

    #[test]
    fn conditional_x_only_on_dir_or_executable() {
        // upstream: +X adds exec on dirs and already-executable files only.
        assert_eq!(apply("a+X", 0o644, false), 0o644);
        assert_eq!(apply("a+X", 0o744, false), 0o755);
        assert_eq!(apply("a+X", 0o600, true), 0o711);
    }

    #[test]
    fn clauses_apply_left_to_right() {
        assert_eq!(apply("000,u+rwx", 0o777, false), 0o700);
        assert_eq!(apply("644,755", 0o000, false), 0o755);
    }

    #[test]
    fn implied_who_masked_by_umask() {
        // +w with umask 022 grants owner-write only.
        assert_eq!(apply("+w", 0o644, false) & 0o022, 0);
    }

    #[test]
    fn file_type_bits_preserved() {
        // Non-permission bits above CHMOD_BITS survive unchanged.
        let clauses = parse_with_umask("644", UMASK).expect("parses");
        let out = tweak_mode(&clauses, 0o100_0777, false);
        assert_eq!(out & !CHMOD_BITS, 0o100_0000);
        assert_eq!(out & CHMOD_BITS, 0o644);
    }
}
