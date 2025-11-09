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
