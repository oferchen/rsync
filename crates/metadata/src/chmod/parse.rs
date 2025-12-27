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

    loop {
        match chars.peek().copied() {
            Some('D') | Some('d') => {
                target = TargetSelector::Directories;
                chars.next();
            }
            Some('F') | Some('f') => {
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

    loop {
        match chars.peek().copied() {
            Some('u') | Some('U') => {
                who.user = true;
                consumed_who = true;
                chars.next();
            }
            Some('g') | Some('G') => {
                who.group = true;
                consumed_who = true;
                chars.next();
            }
            Some('o') | Some('O') => {
                who.other = true;
                consumed_who = true;
                chars.next();
            }
            Some('a') | Some('A') => {
                who = WhoMask::new(true, true, true);
                consumed_who = true;
                chars.next();
            }
            _ => break,
        }
    }

    if !consumed_who {
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

    for ch in perms.chars() {
        match ch {
            'r' | 'R' => spec.read = true,
            'w' | 'W' => spec.write = true,
            'x' => spec.exec = true,
            'X' => spec.exec_if_conditional = true,
            's' | 'S' => {
                spec.setuid = true;
                spec.setgid = true;
            }
            't' | 'T' => spec.sticky = true,
            'u' | 'U' => spec.copy_user = true,
            'g' | 'G' => spec.copy_group = true,
            'o' | 'O' => spec.copy_other = true,
            _ => return Err(ChmodError::new(format!("unsupported chmod token '{ch}'"))),
        }
    }

    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for parse_spec
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

    // Tests for parse_clause - numeric
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

    // Tests for parse_clause - symbolic
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
        // "=" with no perms is valid for assign operation
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

    // Tests for parse_numeric_clause
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
        // 8 and 9 are not valid octal digits
        assert!(parse_numeric_clause("789").is_err());
    }

    // Tests for parse_perm_spec
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
    fn parse_perm_spec_copy_user() {
        let spec = parse_perm_spec("u", Operation::Add).unwrap();
        assert!(spec.copy_user);
    }

    #[test]
    fn parse_perm_spec_copy_group() {
        let spec = parse_perm_spec("g", Operation::Add).unwrap();
        assert!(spec.copy_group);
    }

    #[test]
    fn parse_perm_spec_copy_other() {
        let spec = parse_perm_spec("o", Operation::Add).unwrap();
        assert!(spec.copy_other);
    }

    #[test]
    fn parse_perm_spec_multiple() {
        let spec = parse_perm_spec("rwx", Operation::Add).unwrap();
        assert!(spec.read);
        assert!(spec.write);
        assert!(spec.exec);
    }

    #[test]
    fn parse_perm_spec_uppercase() {
        let spec = parse_perm_spec("RWX", Operation::Add).unwrap();
        assert!(spec.read);
        assert!(spec.write);
        // Note: X is conditional exec, not regular exec
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
}
