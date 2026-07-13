//! Parser for `--chmod` specifications.
//!
//! Faithful port of upstream rsync's `chmod.c:parse_chmod()` state machine. The
//! whole modestring is scanned in a single pass so that comma handling, the
//! `D`/`F` selectors, octal modes, and the symbolic `[ugoa][-+=][rwxXst]` forms
//! match upstream byte for byte, including the error transitions. A category
//! letter (`u`/`g`/`o`) in the second half is rejected exactly as upstream does
//! (chmod.c STATE_2ND_HALF `default:` -> STATE_ERROR): rsync's `--chmod` grammar
//! has no chmod(1)-style copy-from-category form. Each clause is reduced to an
//! AND/OR pair consumed by the evaluator in `apply.rs`.

use super::{ChmodError, spec::CHMOD_BITS, spec::Clause};

// upstream: chmod.c CHMOD_ADD / CHMOD_SUB / CHMOD_EQ / CHMOD_SET.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Op {
    None,
    Add,
    Sub,
    Eq,
    Set,
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
    let mut op = Op::None;
    let mut topbits: u32 = 0;
    let mut topoct: u32 = 0;
    let mut x_keep = false;
    let mut dirs_only = false;
    let mut files_only = false;

    let err = |c: char| ChmodError::new(format!("invalid --chmod specification: '{c}'"));

    loop {
        let ch = bytes.get(i).copied();

        // upstream: chmod.c:58 - at end-of-string or a comma, close the clause.
        if ch.is_none() || ch == Some(b',') {
            if op == Op::None {
                return Err(ChmodError::new(
                    "invalid --chmod specification: empty clause",
                ));
            }

            // upstream: chmod.c:73-78.
            let bits = if where_ != 0 {
                where_ * what
            } else {
                where_ = 0o111;
                (where_ * what) & !umask
            };

            // upstream: chmod.c:80-97.
            let (mode_and, mode_or) = match op {
                Op::Add => (CHMOD_BITS, bits + topoct),
                Op::Sub => (CHMOD_BITS - bits - topoct, 0),
                Op::Eq => {
                    let special = if topoct != 0 { topbits } else { 0 };
                    (CHMOD_BITS - (where_ * 7) - special, bits + topoct)
                }
                Op::Set => (0, bits),
                Op::None => unreachable!(),
            };

            clauses.push(Clause {
                mode_and,
                mode_or,
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
            op = Op::None;
            topbits = 0;
            topoct = 0;
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
                b'a' => where_ |= 0o111,
                b'+' => {
                    op = Op::Add;
                    state = State::SecondHalf;
                }
                b'-' => {
                    op = Op::Sub;
                    state = State::SecondHalf;
                }
                b'=' => {
                    op = Op::Eq;
                    state = State::SecondHalf;
                }
                _ => {
                    // upstream: chmod.c:148-156 - an octal digit (< 8) starts a
                    // numeric mode only when no who-class has been seen.
                    if byte.is_ascii_digit() && byte < b'8' && where_ == 0 {
                        op = Op::Set;
                        state = State::OctalNum;
                        where_ = 1;
                        what = u32::from(byte - b'0');
                    } else {
                        return Err(err(c));
                    }
                }
            },
            // upstream: chmod.c:159-185 STATE_2ND_HALF. Only `rwxXst` are valid;
            // anything else (including a `u`/`g`/`o` category letter - rsync has
            // no chmod(1)-style copy-from-category form) hits `default:` and
            // routes to STATE_ERROR.
            State::SecondHalf => match byte {
                b'r' => what |= 4,
                b'w' => what |= 2,
                b'X' => {
                    x_keep = true;
                    what |= 1;
                }
                b'x' => what |= 1,
                b's' => {
                    if topbits != 0 {
                        topoct |= topbits;
                    } else {
                        topoct = 0o4000;
                    }
                }
                b't' => topoct |= 0o1000,
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
        // upstream: chmod.c:173-178 - `s` sets `topoct = 04000` when no u/g
        // who-letter contributed to topbits (o/a/implied who).
        assert_eq!(one("o+s").mode_or, 0o4000);
        assert_eq!(one("a+s").mode_or, 0o4000);
        assert_eq!(one("+s").mode_or, 0o4000);
        // u/g who select their own top bit.
        assert_eq!(one("u+s").mode_or, 0o4000);
        assert_eq!(one("g+s").mode_or, 0o2000);
        assert_eq!(one("ug+s").mode_or, 0o6000);
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
    fn copy_from_category_forms_rejected() {
        // upstream: chmod.c:159-185 STATE_2ND_HALF has no `u`/`g`/`o` case, so a
        // category letter in the permission half falls to `default:` ->
        // STATE_ERROR and parse_chmod returns NULL. rsync's `--chmod` grammar
        // does not implement chmod(1)'s copy-from-category form; the caller emits
        // `Invalid argument passed to --chmod (<spec>)` and exits RERR_SYNTAX.
        for spec in ["g=u", "o=g", "u+g", "a=u", "g-o", "o=u", "g=ur", "g=us"] {
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
