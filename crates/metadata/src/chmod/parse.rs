//! Parser for `--chmod` specifications.
//!
//! Splits comma-separated clauses, recognises optional `D`/`F` target
//! selectors, and dispatches each clause to the numeric or symbolic
//! parser. Errors produce [`ChmodError`] with the offending clause text.

use super::{
    ChmodError,
    spec::{
        Clause, ClauseKind, NumericClause, Operation, PermSpec, SymbolicClause, TargetSelector,
        WhoMask,
    },
};

pub(crate) fn parse_spec(spec: &str) -> Result<Vec<Clause>, ChmodError> {
    let mut clauses = Vec::new();

    for part in spec.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }

        let clause = parse_clause(trimmed)?;
        clauses.push(clause);
    }

    Ok(clauses)
}

fn parse_clause(text: &str) -> Result<Clause, ChmodError> {
    let mut chars = text.chars().peekable();
    let mut target = TargetSelector::All;

    // upstream: chmod.c:parse_chmod() STATE_1ST_HALF - only uppercase `D`
    // (dirs-only) and `F` (files-only) are target flags. Lowercase `d`/`f`
    // are not accepted; upstream routes them to STATE_ERROR.
    loop {
        match chars.peek().copied() {
            Some('D') => {
                target = TargetSelector::Directories;
                chars.next();
            }
            Some('F') => {
                target = TargetSelector::Files;
                chars.next();
            }
            _ => break,
        }
    }

    let remaining: String = chars.clone().collect();
    if remaining.chars().all(|ch| ch.is_ascii_digit()) && !remaining.is_empty() {
        let value = parse_numeric_clause(&remaining)?;
        return Ok(Clause {
            kind: ClauseKind::Numeric(NumericClause {
                target,
                mode: value,
            }),
        });
    }

    let mut who = WhoMask::new(false, false, false);
    let mut consumed_who = false;

    // upstream: chmod.c:parse_chmod() STATE_1ST_HALF - the who-class letters
    // are lowercase only (`u`, `g`, `o`, `a`). Uppercase variants are not
    // accepted and fall through to STATE_ERROR.
    loop {
        match chars.peek().copied() {
            Some('u') => {
                who.user = true;
                consumed_who = true;
                chars.next();
            }
            Some('g') => {
                who.group = true;
                consumed_who = true;
                chars.next();
            }
            Some('o') => {
                who.other = true;
                consumed_who = true;
                chars.next();
            }
            Some('a') => {
                who = WhoMask::new(true, true, true);
                consumed_who = true;
                chars.next();
            }
            _ => break,
        }
    }

    let who_implied = !consumed_who;
    if who_implied {
        who = WhoMask::new(true, true, true);
    }

    let op = match chars.next() {
        Some('+') => Operation::Add,
        Some('-') => Operation::Remove,
        Some('=') => Operation::Assign,
        _ => {
            return Err(ChmodError::new(format!(
                "chmod clause '{text}' is missing a valid operator"
            )));
        }
    };

    let perms: String = chars.collect();
    let spec = parse_perm_spec(&perms, op)?;

    Ok(Clause {
        kind: ClauseKind::Symbolic(SymbolicClause {
            target,
            op,
            who,
            perms: spec,
            who_implied,
        }),
    })
}

fn parse_numeric_clause(text: &str) -> Result<u16, ChmodError> {
    if !(3..=4).contains(&text.len()) {
        return Err(ChmodError::new(format!(
            "numeric chmod '{text}' must contain 3 or 4 octal digits"
        )));
    }

    u16::from_str_radix(text, 8)
        .map_err(|_| ChmodError::new(format!("invalid numeric chmod value '{text}'")))
}

fn parse_perm_spec(perms: &str, op: Operation) -> Result<PermSpec, ChmodError> {
    let mut spec = PermSpec::default();

    if perms.is_empty() {
        if matches!(op, Operation::Assign) {
            return Ok(spec);
        }
        return Err(ChmodError::new(
            "chmod symbolic clause must specify permissions",
        ));
    }

    // upstream: chmod.c:parse_chmod() STATE_2ND_HALF - the permission letters
    // are exactly `r`, `w`, `x`, `X`, `s`, `t`. Anything else (including the
    // who-class letters `u`/`g`/`o`/`a` and uppercase `R`/`W`/`S`/`T`) is
    // rejected via STATE_ERROR. rsync has no GNU-style "copy permissions"
    // form, so `g=ur` must be an error rather than a copy directive.
    for ch in perms.chars() {
        match ch {
            'r' => spec.read = true,
            'w' => spec.write = true,
            'x' => spec.exec = true,
            'X' => spec.exec_if_conditional = true,
            's' => {
                spec.setuid = true;
                spec.setgid = true;
            }
            't' => spec.sticky = true,
            _ => return Err(ChmodError::new(format!("unsupported chmod token '{ch}'"))),
        }
    }

    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_spec_single_numeric() {
        let clauses = parse_spec("755").unwrap();
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Numeric(num) => {
                assert_eq!(num.mode, 0o755);
                assert_eq!(num.target, TargetSelector::All);
            }
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn parse_spec_multiple_clauses() {
        let clauses = parse_spec("u+r,g+w,o-x").unwrap();
        assert_eq!(clauses.len(), 3);
    }

    #[test]
    fn parse_spec_empty_parts_ignored() {
        let clauses = parse_spec("u+r,,g+w").unwrap();
        assert_eq!(clauses.len(), 2);
    }

    #[test]
    fn parse_spec_whitespace_trimmed() {
        let clauses = parse_spec(" u+r , g+w ").unwrap();
        assert_eq!(clauses.len(), 2);
    }

    #[test]
    fn parse_spec_empty_string() {
        let clauses = parse_spec("").unwrap();
        assert!(clauses.is_empty());
    }

    #[test]
    fn parse_clause_numeric_three_digits() {
        let clause = parse_clause("644").unwrap();
        match clause.kind {
            ClauseKind::Numeric(num) => assert_eq!(num.mode, 0o644),
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn parse_clause_numeric_four_digits() {
        let clause = parse_clause("4755").unwrap();
        match clause.kind {
            ClauseKind::Numeric(num) => assert_eq!(num.mode, 0o4755),
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn parse_clause_numeric_with_directory_target() {
        let clause = parse_clause("D755").unwrap();
        match clause.kind {
            ClauseKind::Numeric(num) => {
                assert_eq!(num.mode, 0o755);
                assert_eq!(num.target, TargetSelector::Directories);
            }
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn parse_clause_numeric_with_file_target() {
        let clause = parse_clause("F644").unwrap();
        match clause.kind {
            ClauseKind::Numeric(num) => {
                assert_eq!(num.mode, 0o644);
                assert_eq!(num.target, TargetSelector::Files);
            }
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_user_add_read() {
        let clause = parse_clause("u+r").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Add);
                assert!(sym.who.user);
                assert!(!sym.who.group);
                assert!(!sym.who.other);
                assert!(sym.perms.read);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_group_remove_write() {
        let clause = parse_clause("g-w").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Remove);
                assert!(!sym.who.user);
                assert!(sym.who.group);
                assert!(!sym.who.other);
                assert!(sym.perms.write);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_other_assign_exec() {
        let clause = parse_clause("o=x").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Assign);
                assert!(!sym.who.user);
                assert!(!sym.who.group);
                assert!(sym.who.other);
                assert!(sym.perms.exec);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_all_add_rwx() {
        let clause = parse_clause("a+rwx").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert!(sym.who.user);
                assert!(sym.who.group);
                assert!(sym.who.other);
                assert!(sym.perms.read);
                assert!(sym.perms.write);
                assert!(sym.perms.exec);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_no_who_defaults_to_all() {
        let clause = parse_clause("+x").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert!(sym.who.user);
                assert!(sym.who.group);
                assert!(sym.who.other);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_multiple_who() {
        let clause = parse_clause("ug+r").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert!(sym.who.user);
                assert!(sym.who.group);
                assert!(!sym.who.other);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_with_directory_target() {
        let clause = parse_clause("Du+x").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::Directories);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_with_file_target() {
        let clause = parse_clause("Fu-x").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::Files);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_assign_empty_perms() {
        let clause = parse_clause("u=").unwrap();
        match clause.kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Assign);
                assert!(!sym.perms.read);
                assert!(!sym.perms.write);
                assert!(!sym.perms.exec);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn parse_clause_symbolic_add_empty_perms_fails() {
        let result = parse_clause("u+");
        assert!(result.is_err());
    }

    #[test]
    fn parse_clause_missing_operator_fails() {
        let result = parse_clause("urw");
        assert!(result.is_err());
    }

    #[test]
    fn parse_numeric_clause_valid_three_digits() {
        assert_eq!(parse_numeric_clause("755").unwrap(), 0o755);
        assert_eq!(parse_numeric_clause("644").unwrap(), 0o644);
        assert_eq!(parse_numeric_clause("000").unwrap(), 0);
        assert_eq!(parse_numeric_clause("777").unwrap(), 0o777);
    }

    #[test]
    fn parse_numeric_clause_valid_four_digits() {
        assert_eq!(parse_numeric_clause("4755").unwrap(), 0o4755);
        assert_eq!(parse_numeric_clause("2755").unwrap(), 0o2755);
        assert_eq!(parse_numeric_clause("1777").unwrap(), 0o1777);
    }

    #[test]
    fn parse_numeric_clause_too_short() {
        assert!(parse_numeric_clause("75").is_err());
        assert!(parse_numeric_clause("7").is_err());
    }

    #[test]
    fn parse_numeric_clause_too_long() {
        assert!(parse_numeric_clause("75555").is_err());
    }

    #[test]
    fn parse_numeric_clause_invalid_octal() {
        assert!(parse_numeric_clause("789").is_err());
    }

    #[test]
    fn parse_perm_spec_read() {
        let spec = parse_perm_spec("r", Operation::Add).unwrap();
        assert!(spec.read);
        assert!(!spec.write);
        assert!(!spec.exec);
    }

    #[test]
    fn parse_perm_spec_write() {
        let spec = parse_perm_spec("w", Operation::Add).unwrap();
        assert!(spec.write);
    }

    #[test]
    fn parse_perm_spec_exec() {
        let spec = parse_perm_spec("x", Operation::Add).unwrap();
        assert!(spec.exec);
        assert!(!spec.exec_if_conditional);
    }

    #[test]
    fn parse_perm_spec_exec_conditional() {
        let spec = parse_perm_spec("X", Operation::Add).unwrap();
        assert!(!spec.exec);
        assert!(spec.exec_if_conditional);
    }

    #[test]
    fn parse_perm_spec_setuid_setgid() {
        let spec = parse_perm_spec("s", Operation::Add).unwrap();
        assert!(spec.setuid);
        assert!(spec.setgid);
    }

    #[test]
    fn parse_perm_spec_sticky() {
        let spec = parse_perm_spec("t", Operation::Add).unwrap();
        assert!(spec.sticky);
    }

    #[test]
    fn parse_perm_spec_who_letters_rejected() {
        // upstream: chmod.c:parse_chmod() STATE_2ND_HALF rejects the who-class
        // letters u/g/o/a as permission bits. rsync has no GNU "copy" form.
        assert!(parse_perm_spec("u", Operation::Add).is_err());
        assert!(parse_perm_spec("g", Operation::Add).is_err());
        assert!(parse_perm_spec("o", Operation::Add).is_err());
        assert!(parse_perm_spec("a", Operation::Add).is_err());
    }

    #[test]
    fn parse_perm_spec_multiple() {
        let spec = parse_perm_spec("rwx", Operation::Add).unwrap();
        assert!(spec.read);
        assert!(spec.write);
        assert!(spec.exec);
    }

    #[test]
    fn parse_perm_spec_uppercase_rwst_rejected() {
        // upstream: chmod.c:parse_chmod() STATE_2ND_HALF only accepts uppercase
        // `X`; uppercase R/W/S/T are not permission letters and error out.
        assert!(parse_perm_spec("R", Operation::Add).is_err());
        assert!(parse_perm_spec("W", Operation::Add).is_err());
        assert!(parse_perm_spec("S", Operation::Add).is_err());
        assert!(parse_perm_spec("T", Operation::Add).is_err());
        // `X` remains valid (conditional exec).
        let spec = parse_perm_spec("X", Operation::Add).unwrap();
        assert!(spec.exec_if_conditional);
    }

    #[test]
    fn parse_perm_spec_empty_assign_ok() {
        let spec = parse_perm_spec("", Operation::Assign).unwrap();
        assert!(!spec.read);
        assert!(!spec.write);
        assert!(!spec.exec);
    }

    #[test]
    fn parse_perm_spec_empty_add_fails() {
        let result = parse_perm_spec("", Operation::Add);
        assert!(result.is_err());
    }

    #[test]
    fn parse_perm_spec_empty_remove_fails() {
        let result = parse_perm_spec("", Operation::Remove);
        assert!(result.is_err());
    }

    #[test]
    fn parse_perm_spec_invalid_char_fails() {
        let result = parse_perm_spec("rwz", Operation::Add);
        assert!(result.is_err());
    }

    // Upstream chmod-option.test corpus.
    //
    // These specs are taken verbatim from
    // `testsuite/chmod-option.test` so any drift from upstream's
    // `chmod.c::parse_chmod()` surfaces here instead of in interop.

    #[test]
    fn upstream_f600_parses_as_files_only_numeric_clause() {
        // upstream: chmod-option.test - daemon module exercises `Fo-x`
        // alongside ad-hoc CLI specs like `F600` in interop suites.
        let clauses = parse_spec("F600").expect("parses");
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Numeric(num) => {
                assert_eq!(num.mode, 0o600);
                assert_eq!(num.target, TargetSelector::Files);
            }
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn upstream_d755_parses_as_directories_only_numeric_clause() {
        let clauses = parse_spec("D755").expect("parses");
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Numeric(num) => {
                assert_eq!(num.mode, 0o755);
                assert_eq!(num.target, TargetSelector::Directories);
            }
            _ => panic!("expected numeric clause"),
        }
    }

    #[test]
    fn upstream_fu_eq_rw_go_minus_rwx_parses_two_symbolic_clauses() {
        // upstream: `Fu=rw,go-rwx` produces two clauses. parse_chmod resets
        // `flags = 0` after each `,` (chmod.c:106), so the leading `F` only
        // targets the first clause; the second clause defaults to all targets.
        let clauses = parse_spec("Fu=rw,go-rwx").expect("parses");
        assert_eq!(clauses.len(), 2);
        match &clauses[0].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::Files);
                assert_eq!(sym.op, Operation::Assign);
                assert!(sym.who.user);
                assert!(!sym.who.group);
                assert!(!sym.who.other);
                assert!(sym.perms.read);
                assert!(sym.perms.write);
                assert!(!sym.perms.exec);
            }
            _ => panic!("expected symbolic clause for u=rw"),
        }
        match &clauses[1].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::All);
                assert_eq!(sym.op, Operation::Remove);
                assert!(!sym.who.user);
                assert!(sym.who.group);
                assert!(sym.who.other);
                assert!(sym.perms.read);
                assert!(sym.perms.write);
                assert!(sym.perms.exec);
            }
            _ => panic!("expected symbolic clause for go-rwx"),
        }
    }

    #[test]
    fn upstream_dg_plus_s_parses_directory_only_setgid_add() {
        // upstream: `Dg+s` is the canonical recipe for new directories to
        // inherit their parent group. Verify the parser tags it as
        // directories-only, group who-class, add op, with the setgid flag set
        // so `apply_special_bits` can fold it into the mode.
        let clauses = parse_spec("Dg+s").expect("parses");
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::Directories);
                assert_eq!(sym.op, Operation::Add);
                assert!(sym.who.group);
                assert!(!sym.who.user);
                assert!(!sym.who.other);
                assert!(sym.perms.setgid);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn upstream_plus_x_parses_as_conditional_exec_for_all() {
        // upstream: bare `+X` (no who-class) marks the implied-all who and
        // sets FLAG_X_KEEP via PermSpec::exec_if_conditional.
        let clauses = parse_spec("+X").expect("parses");
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::All);
                assert_eq!(sym.op, Operation::Add);
                assert!(sym.who_implied);
                assert!(sym.perms.exec_if_conditional);
                assert!(!sym.perms.exec);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn upstream_fo_minus_x_parses_files_only_other_remove_exec() {
        // upstream: chmod-option.test daemon module - `incoming chmod = Fo-x`.
        let clauses = parse_spec("Fo-x").expect("parses");
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::Files);
                assert_eq!(sym.op, Operation::Remove);
                assert!(!sym.who.user);
                assert!(!sym.who.group);
                assert!(sym.who.other);
                assert!(sym.perms.exec);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn upstream_ug_minus_s_a_plus_rx_d_plus_w_parses_three_clauses() {
        // upstream: `--chmod ug-s,a+rX,D+w` from chmod-option.test.
        let clauses = parse_spec("ug-s,a+rX,D+w").expect("parses");
        assert_eq!(clauses.len(), 3);
        match &clauses[0].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Remove);
                assert!(sym.who.user);
                assert!(sym.who.group);
                assert!(!sym.who.other);
                assert!(sym.perms.setuid);
                assert!(sym.perms.setgid);
            }
            _ => panic!("expected symbolic clause for ug-s"),
        }
        match &clauses[1].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Add);
                assert!(sym.who.user && sym.who.group && sym.who.other);
                assert!(sym.perms.read);
                assert!(sym.perms.exec_if_conditional);
            }
            _ => panic!("expected symbolic clause for a+rX"),
        }
        match &clauses[2].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.target, TargetSelector::Directories);
                assert_eq!(sym.op, Operation::Add);
                assert!(sym.who_implied);
                assert!(sym.perms.write);
            }
            _ => panic!("expected symbolic clause for D+w"),
        }
    }

    #[test]
    fn upstream_eq_with_empty_rhs_is_reset_to_zero() {
        // upstream: `u=` clears all user-class permission bits (CHMOD_EQ with
        // empty bits resolves to ModeAND that strips the who-class and
        // ModeOR of 0).
        let clauses = parse_spec("u=").expect("parses");
        assert_eq!(clauses.len(), 1);
        match &clauses[0].kind {
            ClauseKind::Symbolic(sym) => {
                assert_eq!(sym.op, Operation::Assign);
                assert!(sym.who.user);
                assert!(!sym.who.group);
                assert!(!sym.who.other);
                assert!(!sym.perms.read);
                assert!(!sym.perms.write);
                assert!(!sym.perms.exec);
            }
            _ => panic!("expected symbolic clause"),
        }
    }

    #[test]
    fn upstream_g_eq_ur_is_rejected() {
        // upstream: chmod.c:parse_chmod() STATE_2ND_HALF sees `u` after `g=`
        // and transitions to STATE_ERROR, so parse_chmod returns NULL. rsync
        // rejects `g=ur`; oc-rsync must not silently accept it as a copy form.
        assert!(parse_spec("g=ur").is_err());
        // Equivalent who-letter-in-RHS mixes are likewise rejected.
        assert!(parse_spec("u+g").is_err());
        assert!(parse_spec("o=g").is_err());
        assert!(parse_spec("a=u").is_err());
    }

    #[test]
    fn upstream_valid_symbolic_forms_still_parse() {
        // Guard against over-rejection: the canonical valid specs from
        // chmod-option.test must continue to parse cleanly.
        for spec in ["Fugo=rwx", "g-w", "Dg+s", "u+rwx", "Fo-x", "ug-s", "a+rX"] {
            parse_spec(spec).unwrap_or_else(|_| panic!("`{spec}` must parse"));
        }
    }

    #[test]
    fn upstream_lowercase_target_flags_rejected() {
        // upstream: only uppercase `D`/`F` are target flags; lowercase `d`/`f`
        // are not who-class letters, digits, or operators -> STATE_ERROR.
        assert!(parse_spec("d755").is_err());
        assert!(parse_spec("fu+x").is_err());
    }
}
