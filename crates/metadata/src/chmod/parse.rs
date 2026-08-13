//! Parser for `--chmod` specifications.
//!
//! Faithful port of upstream rsync's `chmod.c:parse_chmod()` state machine. The
//! whole modestring is scanned in a single pass so that comma handling, the
//! `D`/`F` selectors, octal modes, the symbolic `[ugoa][-+=][rwxXst]` forms and
//! the chmod(1)-style permission copies `[ugoa][-+=][ugo]` match upstream byte
//! for byte, including the error transitions. Each clause is reduced to an
//! AND/OR pair plus a permission-copy triple, consumed by the evaluator in
//! `apply.rs`.

use super::{ChmodError, spec::CHMOD_BITS, spec::Clause, spec::Op};

/// Set-id/sticky bits owned by the who-classes in `where_`.
///
/// upstream: chmod.c `mode_dest_special_bits()` - a `=` clause that copies
/// permissions also clears the special bit each destination class owns, so
/// `u=g` drops setuid, `g=o` drops setgid and `o=u` drops the sticky bit.
const fn dest_special_bits(where_: u32) -> u32 {
    let mut bits = 0;
    if where_ & 0o100 != 0 {
        bits |= 0o4000;
    }
    if where_ & 0o010 != 0 {
        bits |= 0o2000;
    }
    if where_ & 0o001 != 0 {
        bits |= 0o1000;
    }
    bits
}

// upstream: chmod.c STATE_1ST_HALF / STATE_2ND_HALF / STATE_OCTAL_NUM.
#[derive(Clone, Copy, Eq, PartialEq)]
enum State {
    FirstHalf,
    SecondHalf,
    OctalNum,
}

/// Returns the process umask, cached for the lifetime of the process.
///
/// upstream: `main.c` captures `orig_umask` once at startup and
/// `chmod.c:parse_chmod()` folds `~orig_umask` into clauses whose who-class is
/// implied. We read it lazily and cache it so the set-and-restore syscall pair
/// happens at most once.
#[cfg(unix)]
#[allow(unsafe_code)]
fn orig_umask() -> u32 {
    use std::sync::OnceLock;
    static UMASK: OnceLock<u32> = OnceLock::new();
    *UMASK.get_or_init(|| {
        // SAFETY: umask is a standard POSIX call. Setting it to 0 reads the
        // current value, which we immediately restore. The OnceLock guarantees
        // this pair runs at most once per process, leaving no window for a
        // concurrent umask change.
        let old = unsafe { libc::umask(0) };
        unsafe { libc::umask(old) };
        old as u32
    })
}

#[cfg(not(unix))]
fn orig_umask() -> u32 {
    0
}

/// Parses a `--chmod` specification against the process umask.
pub(crate) fn parse_spec(modestr: &str) -> Result<Vec<Clause>, ChmodError> {
    parse_with_umask(modestr, orig_umask())
}

/// Parses `modestr` using an explicit `umask`, mirroring
/// `chmod.c:parse_chmod()`.
pub(crate) fn parse_with_umask(modestr: &str, umask: u32) -> Result<Vec<Clause>, ChmodError> {
    let bytes = modestr.as_bytes();
    let mut i = 0usize;
    let mut clauses = Vec::new();

    let mut state = State::FirstHalf;
    let mut where_: u32 = 0;
    let mut what: u32 = 0;
    let mut op: Option<Op> = None;
    let mut topbits: u32 = 0;
    let mut topoct: u32 = 0;
    let mut copybits: u32 = 0;
    let mut x_keep = false;
    let mut dirs_only = false;
    let mut files_only = false;

    let err = |c: char| ChmodError::new(format!("invalid --chmod specification: '{c}'"));

    loop {
        let ch = bytes.get(i).copied();

        // upstream: chmod.c:58 - at end-of-string or a comma, close the clause.
        if ch.is_none() || ch == Some(b',') {
            let Some(clause_op) = op else {
                return Err(ChmodError::new(
                    "invalid --chmod specification: empty clause",
                ));
            };

            // upstream: chmod.c parse_chmod() - an omitted who-class becomes
            // `a` and folds `~orig_umask` into both the literal and the copied
            // bits (`ModeCOPY_AND`).
            let where_specified = where_ != 0;
            let bits = if where_specified {
                where_ * what
            } else {
                where_ = 0o111;
                (where_ * what) & !umask
            };
            let mode_copy_and = if where_specified { CHMOD_BITS } else { !umask };

            // upstream: chmod.c parse_chmod() `switch (op)`.
            let (mode_and, mode_or) = match clause_op {
                Op::Add => (CHMOD_BITS, bits + topoct),
                Op::Sub => (CHMOD_BITS - bits - topoct, 0),
                Op::Eq => {
                    let special = if topoct != 0 { topbits } else { 0 };
                    // A copy also clears the special bit each destination class
                    // owns, so `u=g` drops setuid. upstream:
                    // chmod.c `mode_dest_special_bits()`.
                    let copy_special = if copybits != 0 {
                        dest_special_bits(where_)
                    } else {
                        0
                    };
                    (
                        CHMOD_BITS - (where_ * 7) - special - copy_special,
                        bits + topoct,
                    )
                }
                Op::Set => (0, bits),
            };
            // A numeric mode never copies. upstream: the CHMOD_SET arm zeroes
            // the copy triple and pins ModeCOPY_AND to CHMOD_BITS.
            let (copy_src, copy_dst, copy_and) = match clause_op {
                Op::Set => (0, 0, CHMOD_BITS),
                Op::Add | Op::Sub | Op::Eq => (copybits, where_, mode_copy_and),
            };

            clauses.push(Clause {
                mode_and,
                mode_or,
                copy_src,
                copy_dst,
                copy_and,
                op: clause_op,
                x_keep,
                dirs_only,
                files_only,
            });

            if ch.is_none() {
                break;
            }

            // upstream: chmod.c:103-106 - consume the comma and reset per-clause
            // state (the `D`/`F` selector does not carry across a comma).
            i += 1;
            state = State::FirstHalf;
            where_ = 0;
            what = 0;
            op = None;
            topbits = 0;
            topoct = 0;
            copybits = 0;
            x_keep = false;
            dirs_only = false;
            files_only = false;
            continue;
        }

        let byte = ch.expect("boundary handled above");
        let c = byte as char;

        match state {
            // upstream: chmod.c:110-158.
            State::FirstHalf => match byte {
                b'D' => {
                    if files_only {
                        return Err(err(c));
                    }
                    dirs_only = true;
                }
                b'F' => {
                    if dirs_only {
                        return Err(err(c));
                    }
                    files_only = true;
                }
                b'u' => {
                    where_ |= 0o100;
                    topbits |= 0o4000;
                }
                b'g' => {
                    where_ |= 0o010;
                    topbits |= 0o2000;
                }
                b'o' => where_ |= 0o001,
                b'a' => {
                    where_ |= 0o111;
                    // upstream: chmod.c:parse_chmod() `case 'a'` - `a` covers
                    // both `u` and `g`, so it contributes both set-id bits and
                    // `a+s` sets setuid AND setgid, matching chmod(1). Without
                    // this the `s` clause below falls to its no-topbits default
                    // and sets setuid only.
                    topbits |= 0o6000;
                }
                b'+' => {
                    op = Some(Op::Add);
                    state = State::SecondHalf;
                }
                b'-' => {
                    op = Some(Op::Sub);
                    state = State::SecondHalf;
                }
                b'=' => {
                    op = Some(Op::Eq);
                    state = State::SecondHalf;
                }
                _ => {
                    // upstream: chmod.c:148-156 - an octal digit (< 8) starts a
                    // numeric mode only when no who-class has been seen.
                    if byte.is_ascii_digit() && byte < b'8' && where_ == 0 {
                        op = Some(Op::Set);
                        state = State::OctalNum;
                        where_ = 1;
                        what = u32::from(byte - b'0');
                    } else {
                        return Err(err(c));
                    }
                }
            },
            // upstream: chmod.c parse_chmod() STATE_2ND_HALF. `rwxXst` name
            // literal bits; a lone `u`/`g`/`o` names a permission CLASS to copy
            // from. The two are mutually exclusive and at most one copy letter
            // is allowed, so every guard below routes to STATE_ERROR. `a` is
            // not a copy source and falls to `default:`.
            State::SecondHalf => match byte {
                b'r' | b'w' | b'x' | b'X' if copybits != 0 => return Err(err(c)),
                b'r' => what |= 4,
                b'w' => what |= 2,
                b'X' => {
                    x_keep = true;
                    what |= 1;
                }
                b'x' => what |= 1,
                b's' | b't' if copybits != 0 => return Err(err(c)),
                b's' => {
                    if topbits != 0 {
                        topoct |= topbits;
                    } else {
                        topoct = 0o4000;
                    }
                }
                b't' => topoct |= 0o1000,
                b'u' | b'g' | b'o' if what != 0 || topoct != 0 || copybits != 0 => {
                    return Err(err(c));
                }
                b'u' => copybits = 0o100,
                b'g' => copybits = 0o010,
                b'o' => copybits = 0o001,
                _ => return Err(err(c)),
            },
            // upstream: chmod.c:187-194.
            State::OctalNum => {
                if byte.is_ascii_digit() && byte < b'8' {
                    what = what * 8 + u32::from(byte - b'0');
                    if what > CHMOD_BITS {
                        return Err(err(c));
                    }
                } else {
                    return Err(err(c));
                }
            }
        }

        i += 1;
    }

    Ok(clauses)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Deterministic umask so folded implied-who bits are reproducible.
    const UMASK: u32 = 0o022;

    fn parse(spec: &str) -> Result<Vec<Clause>, ChmodError> {
        parse_with_umask(spec, UMASK)
    }

    fn one(spec: &str) -> Clause {
        let clauses = parse(spec).expect("parses");
        assert_eq!(clauses.len(), 1, "expected one clause for `{spec}`");
        clauses[0]
    }

    #[test]
    fn octal_set_clears_and_sets() {
        // upstream: chmod.c CHMOD_SET - ModeAND=0, ModeOR=octal value.
        let c = one("750");
        assert_eq!(c.mode_and, 0);
        assert_eq!(c.mode_or, 0o750);
    }

    #[test]
    fn octal_accepts_one_to_four_digits_capped_at_chmod_bits() {
        // upstream: chmod.c:187-194 accumulates octal digits, capping at
        // CHMOD_BITS. Length is not fixed at 3-4 digits.
        assert_eq!(one("7").mode_or, 0o7);
        assert_eq!(one("75").mode_or, 0o75);
        assert_eq!(one("0644").mode_or, 0o644);
        assert_eq!(one("00644").mode_or, 0o644);
        assert_eq!(one("4755").mode_or, 0o4755);
        // Overflow past CHMOD_BITS is an error.
        assert!(parse("17777").is_err());
        // 8/9 are not octal digits.
        assert!(parse("789").is_err());
    }

    #[test]
    fn directory_and_file_selectors() {
        let d = one("D755");
        assert!(d.dirs_only && !d.files_only);
        let f = one("F644");
        assert!(f.files_only && !f.dirs_only);
        // upstream: chmod.c:113-120 - mixing D and F in one clause errors.
        assert!(parse("DF644").is_err());
        assert!(parse("FD644").is_err());
    }

    #[test]
    fn selector_resets_after_comma() {
        // upstream: chmod.c:106 resets flags after each comma, so the leading
        // `F` only tags the first clause.
        let clauses = parse("Fu=rw,go-r").expect("parses");
        assert_eq!(clauses.len(), 2);
        assert!(clauses[0].files_only);
        assert!(!clauses[1].files_only && !clauses[1].dirs_only);
    }

    #[test]
    fn add_user_exec() {
        // u+x: ModeAND=CHMOD_BITS, ModeOR=0o100.
        let c = one("u+x");
        assert_eq!(c.mode_and, CHMOD_BITS);
        assert_eq!(c.mode_or, 0o100);
    }

    #[test]
    fn remove_group_write() {
        // g-w: ModeAND=CHMOD_BITS-0o020, ModeOR=0.
        let c = one("g-w");
        assert_eq!(c.mode_and, CHMOD_BITS - 0o020);
        assert_eq!(c.mode_or, 0);
    }

    #[test]
    fn assign_preserves_setid_when_no_s_present() {
        // upstream: chmod.c:90 - CHMOD_EQ only strips the top bits (topbits)
        // when `s`/`t` are present (topoct != 0). `u=rx` keeps setuid.
        let c = one("u=rx");
        assert_eq!(c.mode_and, CHMOD_BITS - 0o700);
        assert_eq!(c.mode_or, 0o500);
    }

    #[test]
    fn setid_default_setuid_without_ug_who() {
        // upstream: chmod.c:parse_chmod() `case 's'` - `s` sets `topoct = 04000`
        // when no who-letter contributed to topbits (`o` and the implied who).
        assert_eq!(one("o+s").mode_or, 0o4000);
        assert_eq!(one("+s").mode_or, 0o4000);
        // u/g who select their own top bit; `a` covers both.
        assert_eq!(one("u+s").mode_or, 0o4000);
        assert_eq!(one("g+s").mode_or, 0o2000);
        assert_eq!(one("ug+s").mode_or, 0o6000);
        assert_eq!(one("a+s").mode_or, 0o6000);
    }

    #[test]
    fn sticky_ignores_who() {
        // upstream: chmod.c:179-181 - `t` always adds 01000 regardless of who.
        assert_eq!(one("g+t").mode_or, 0o1000);
        assert_eq!(one("u+t").mode_or, 0o1000);
        assert_eq!(one("+t").mode_or, 0o1000);
    }

    #[test]
    fn conditional_x_flag_recorded() {
        let c = one("a+rX");
        assert!(c.x_keep);
        // r+x bits present in ModeOR (0o555); the evaluator masks x when needed.
        assert_eq!(c.mode_or, 0o555);
    }

    #[test]
    fn implied_who_applies_umask() {
        // upstream: chmod.c:76-77 - `+w` with no who becomes where=0o111 and is
        // masked by ~umask. With umask 022, only owner-write survives.
        let c = one("+w");
        assert_eq!(c.mode_or, 0o222 & !UMASK);
        assert_eq!(c.mode_or, 0o200);
    }

    #[test]
    fn permission_copy_records_the_source_class() {
        // upstream: chmod.c parse_chmod() STATE_2ND_HALF `case 'u'/'g'/'o'` -
        // a lone category letter sets ModeCOPY_SRC and leaves the literal bits
        // empty; ModeCOPY_DST is the who-class the clause writes to.
        let c = one("u=g");
        assert_eq!((c.copy_src, c.copy_dst), (0o010, 0o100));
        assert_eq!(c.mode_or, 0);
        // `=` also clears the destination class's own special bit (setuid here).
        assert_eq!(c.mode_and, CHMOD_BITS - 0o700 - 0o4000);

        let c = one("go+u");
        assert_eq!((c.copy_src, c.copy_dst), (0o100, 0o011));
        assert_eq!(c.mode_and, CHMOD_BITS);
        // An explicit who-class leaves the copied bits unmasked.
        assert_eq!(c.copy_and, CHMOD_BITS);
    }

    #[test]
    fn implied_who_masks_the_copied_bits_by_umask() {
        // upstream: chmod.c parse_chmod() - ModeCOPY_AND is `~orig_umask` when
        // the who-class was implied, mirroring the literal-bit path.
        assert_eq!(one("+u").copy_and, !UMASK);
        assert_eq!(one("a+u").copy_and, CHMOD_BITS);
    }

    #[test]
    fn copy_source_is_exclusive_within_its_clause() {
        // upstream: chmod.c parse_chmod() STATE_2ND_HALF guards every branch on
        // `copybits`, and the copy branches on `what || topoct || copybits`, so a
        // copy letter may not be mixed with literal bits, set-id/sticky letters,
        // or a second copy letter - in either order. `a` is never a copy source.
        for spec in [
            "u=gr", "u=rg", "u=gs", "u=sg", "u=gt", "u=tg", "u=gX", "u=Xg", "u=ug", "u=gg", "u+a",
            "u=a", "a=a", "+a",
        ] {
            assert!(parse(spec).is_err(), "`{spec}` must be rejected");
        }
    }

    #[test]
    fn empty_and_stray_commas_rejected() {
        // upstream: chmod.c:61-63 - a clause with no operator errors.
        assert!(parse("").is_err());
        assert!(parse("u+r,").is_err());
        assert!(parse(",u+r").is_err());
        assert!(parse("u+r,,g+w").is_err());
    }

    #[test]
    fn whitespace_rejected() {
        // upstream: a space is neither a who-class, operator, nor digit.
        assert!(parse(" u+r").is_err());
        assert!(parse("u+r ").is_err());
    }

    #[test]
    fn missing_operator_and_who_only_rejected() {
        assert!(parse("u").is_err());
        assert!(parse("urw").is_err());
        assert!(parse("D").is_err());
    }

    #[test]
    fn uppercase_perm_letters_rejected() {
        // upstream: only `X` is an uppercase perm letter; R/W/S/T error.
        assert!(parse("a+R").is_err());
        assert!(parse("a+W").is_err());
        assert!(parse("a+S").is_err());
        assert!(parse("a+T").is_err());
    }

    #[test]
    fn multiple_who_multiplies_bits() {
        // ug+r: where=0o110, what=4 -> bits=0o440.
        assert_eq!(one("ug+r").mode_or, 0o440);
    }
}
