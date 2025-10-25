#![allow(clippy::cast_possible_truncation)]

//! Parser and evaluator for receiver-side `--chmod` modifiers.
//!
//! The upstream rsync CLI allows multiple `--chmod=SPEC` occurrences where each
//! specification may contain comma-separated numeric or symbolic clauses. This
//! module mirrors the clause grammar and applies modifiers to permission modes
//! with behaviour identical to GNU `chmod`, including conditional execute bits
//! and copy directives (for example `g=u`). The [`ChmodModifiers`] type wraps
//! the parsed clauses and exposes [`ChmodModifiers::apply`] so higher layers can
//! evaluate modifiers after the standard metadata preservation step.

use std::fmt;

/// Describes the file-kind specific application target for a chmod clause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetSelector {
    All,
    Files,
    Directories,
}

impl TargetSelector {
    #[cfg(unix)]
    fn matches(self, file_type: std::fs::FileType) -> bool {
        match self {
            Self::All => true,
            Self::Files => !file_type.is_dir(),
            Self::Directories => file_type.is_dir(),
        }
    }

    #[cfg(not(unix))]
    fn matches(self, _file_type: std::fs::FileType) -> bool {
        matches!(self, TargetSelector::All | TargetSelector::Files)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operation {
    Add,
    Remove,
    Assign,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WhoMask {
    user: bool,
    group: bool,
    other: bool,
}

impl WhoMask {
    fn new(user: bool, group: bool, other: bool) -> Self {
        Self { user, group, other }
    }

    fn includes_user(self) -> bool {
        self.user
    }

    fn includes_group(self) -> bool {
        self.group
    }

    fn includes_other(self) -> bool {
        self.other
    }

    fn covers_all(self) -> bool {
        self.user && self.group && self.other
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct PermSpec {
    read: bool,
    write: bool,
    exec: bool,
    exec_if_conditional: bool,
    setuid: bool,
    setgid: bool,
    sticky: bool,
    copy_user: bool,
    copy_group: bool,
    copy_other: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SymbolicClause {
    target: TargetSelector,
    op: Operation,
    who: WhoMask,
    perms: PermSpec,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NumericClause {
    target: TargetSelector,
    mode: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ClauseKind {
    Symbolic(SymbolicClause),
    Numeric(NumericClause),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Clause {
    kind: ClauseKind,
}

/// Error produced when parsing a `--chmod` specification fails.
#[derive(Debug, Eq, PartialEq)]
pub struct ChmodError {
    message: String,
}

impl ChmodError {
    fn new(text: impl Into<String>) -> Self {
        Self {
            message: text.into(),
        }
    }
}

impl fmt::Display for ChmodError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ChmodError {}

/// Parsed representation of one or more `--chmod` directives.
#[derive(Clone, Debug, Eq, PartialEq, Default)]
pub struct ChmodModifiers {
    clauses: Vec<Clause>,
}

impl ChmodModifiers {
    /// Parses a comma-separated chmod specification.
    pub fn parse(spec: &str) -> Result<Self, ChmodError> {
        let mut clauses = Vec::new();
        for part in spec.split(',') {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }

            let clause = parse_clause(trimmed)?;
            clauses.push(clause);
        }

        if clauses.is_empty() {
            return Err(ChmodError::new(
                "chmod specification did not contain any clauses",
            ));
        }

        Ok(Self { clauses })
    }

    /// Appends clauses from another [`ChmodModifiers`] value.
    pub fn extend(&mut self, other: ChmodModifiers) {
        self.clauses.extend(other.clauses);
    }

    /// Applies the modifiers to the provided mode, returning the updated value.
    #[cfg(unix)]
    pub fn apply(&self, mode: u32, file_type: std::fs::FileType) -> u32 {
        let mut current = mode;
        for clause in &self.clauses {
            match &clause.kind {
                ClauseKind::Numeric(numeric) => {
                    if !numeric.target.matches(file_type) {
                        continue;
                    }

                    let preserved = current & !0o7777;
                    current = preserved | u32::from(numeric.mode & 0o7777);
                }
                ClauseKind::Symbolic(symbolic) => {
                    if !symbolic.target.matches(file_type) {
                        continue;
                    }
                    current = apply_symbolic_clause(current, symbolic, file_type.is_dir());
                }
            }
        }
        current
    }

    /// Applies the modifiers on non-Unix platforms.
    #[cfg(not(unix))]
    pub fn apply(&self, mode: u32, _file_type: std::fs::FileType) -> u32 {
        let _ = mode;
        mode
    }

    /// Returns `true` when no clauses are present.
    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }
}

#[cfg(unix)]
fn apply_symbolic_clause(mut mode: u32, clause: &SymbolicClause, is_dir: bool) -> u32 {
    let before = mode;

    for dest in [Dest::User, Dest::Group, Dest::Other] {
        if !dest.includes(clause) {
            continue;
        }

        let mask = dest.permission_mask();
        let mut bits = mode & mask;

        if matches!(clause.op, Operation::Assign) {
            bits = 0;
        }

        // Handle copies before direct permission adjustments so the copied bits
        // participate in any explicit removals.
        let mut copied = 0u32;
        if clause.perms.copy_user {
            copied |= copy_from(Dest::User, dest, before);
        }
        if clause.perms.copy_group {
            copied |= copy_from(Dest::Group, dest, before);
        }
        if clause.perms.copy_other {
            copied |= copy_from(Dest::Other, dest, before);
        }

        match clause.op {
            Operation::Add => {
                bits |= copied & mask;
            }
            Operation::Remove => {
                bits &= !(copied & mask);
            }
            Operation::Assign => {
                bits = copied & mask;
            }
        }

        let add_bits = permission_bits(&clause.perms, dest, is_dir, before);
        match clause.op {
            Operation::Add | Operation::Assign => {
                bits |= add_bits & mask;
            }
            Operation::Remove => {
                bits &= !(add_bits & mask);
            }
        }

        mode = (mode & !mask) | (bits & mask);
    }

    mode = apply_special_bits(mode, before, clause, is_dir);
    mode
}

#[cfg(unix)]
fn apply_special_bits(mode: u32, _before: u32, clause: &SymbolicClause, _is_dir: bool) -> u32 {
    let mut result = mode;

    if clause.who.includes_user() {
        result = update_special_bit(result, clause.op, clause.perms.setuid, 0o4000);
    }

    if clause.who.includes_group() {
        result = update_special_bit(result, clause.op, clause.perms.setgid, 0o2000);
    }

    if clause.who.includes_other() || clause.who.covers_all() {
        // Sticky bit behaves like a permission on the "others" set.
        result = update_special_bit(result, clause.op, clause.perms.sticky, 0o1000);
    }

    result
}

#[cfg(unix)]
fn update_special_bit(current: u32, op: Operation, flag_requested: bool, bit: u32) -> u32 {
    match op {
        Operation::Add => {
            if flag_requested {
                current | bit
            } else {
                current
            }
        }
        Operation::Remove => {
            if flag_requested {
                current & !bit
            } else {
                current
            }
        }
        Operation::Assign => {
            if flag_requested {
                (current & !bit) | bit
            } else {
                current & !bit
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Dest {
    User,
    Group,
    Other,
}

impl Dest {
    fn includes(self, clause: &SymbolicClause) -> bool {
        match self {
            Dest::User => clause.who.includes_user(),
            Dest::Group => clause.who.includes_group(),
            Dest::Other => clause.who.includes_other(),
        }
    }

    fn permission_mask(self) -> u32 {
        match self {
            Dest::User => 0o700,
            Dest::Group => 0o070,
            Dest::Other => 0o007,
        }
    }

    fn shift(self) -> u8 {
        match self {
            Dest::User => 6,
            Dest::Group => 3,
            Dest::Other => 0,
        }
    }

    fn read_mask(self) -> u32 {
        match self {
            Dest::User => 0o400,
            Dest::Group => 0o040,
            Dest::Other => 0o004,
        }
    }

    fn write_mask(self) -> u32 {
        match self {
            Dest::User => 0o200,
            Dest::Group => 0o020,
            Dest::Other => 0o002,
        }
    }

    fn exec_mask(self) -> u32 {
        match self {
            Dest::User => 0o100,
            Dest::Group => 0o010,
            Dest::Other => 0o001,
        }
    }
}

#[cfg(unix)]
fn permission_bits(spec: &PermSpec, dest: Dest, is_dir: bool, before: u32) -> u32 {
    let mut bits = 0u32;

    if spec.read {
        bits |= dest.read_mask();
    }
    if spec.write {
        bits |= dest.write_mask();
    }
    if spec.exec {
        bits |= dest.exec_mask();
    }
    if spec.exec_if_conditional {
        let should_apply = is_dir || (before & 0o111) != 0;
        if should_apply {
            bits |= dest.exec_mask();
        }
    }

    bits
}

#[cfg(unix)]
fn copy_from(source: Dest, dest: Dest, before: u32) -> u32 {
    let src_mask = source.permission_mask();
    let src_bits = before & src_mask;
    let shift = source.shift() as i8 - dest.shift() as i8;

    let shifted = if shift == 0 {
        src_bits
    } else if shift > 0 {
        src_bits >> shift as u32
    } else {
        src_bits << (-shift) as u32
    };

    shifted & dest.permission_mask()
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

    #[test]
    fn parse_symbolic_and_numeric_specifications() {
        let modifiers = ChmodModifiers::parse("Fgo-w,D755,ugo=rwX").expect("parse succeeds");
        assert!(!modifiers.is_empty());
    }

    #[test]
    fn parse_rejects_invalid_token() {
        let error = ChmodModifiers::parse("a+q").expect_err("invalid token");
        assert!(error.to_string().contains("unsupported chmod token"));
    }

    #[cfg(unix)]
    #[test]
    fn apply_numeric_and_symbolic_modifiers() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("file.txt");
        let dir_path = temp.path().join("dir");
        std::fs::write(&file_path, b"payload").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");
        std::fs::set_permissions(&file_path, PermissionsExt::from_mode(0o666)).expect("set perms");

        let file_type = std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type();
        let dir_type = std::fs::metadata(&dir_path)
            .expect("dir metadata")
            .file_type();

        let modifiers = ChmodModifiers::parse("Fgo-w,D755").expect("parse");
        let file_mode = modifiers.apply(0o666, file_type);
        assert_eq!(file_mode & 0o777, 0o644);
        let dir_mode = modifiers.apply(0o600, dir_type);
        assert_eq!(dir_mode & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn conditional_execute_bit_behaviour_matches_rsync() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("script.sh");
        let dir_path = temp.path().join("bin");
        std::fs::write(&file_path, b"#!/bin/sh").expect("write file");
        std::fs::create_dir(&dir_path).expect("create dir");

        let file_type = std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type();
        let dir_type = std::fs::metadata(&dir_path)
            .expect("dir metadata")
            .file_type();

        let modifiers = ChmodModifiers::parse("a+X").expect("parse");
        let file_mode = modifiers.apply(0o644, file_type);
        assert_eq!(file_mode & 0o777, 0o644);
        let dir_mode = modifiers.apply(0o600, dir_type);
        assert_eq!(dir_mode & 0o777, 0o711);
    }

    #[cfg(unix)]
    #[test]
    fn user_group_copy_clauses_are_respected() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file_path = temp.path().join("data.bin");
        std::fs::write(&file_path, b"payload").expect("write file");
        let file_type = std::fs::metadata(&file_path)
            .expect("file metadata")
            .file_type();

        let modifiers = ChmodModifiers::parse("g=u,o=g").expect("parse");
        let mode = modifiers.apply(0o640, file_type);
        assert_eq!(mode & 0o777, 0o666);
    }
}
