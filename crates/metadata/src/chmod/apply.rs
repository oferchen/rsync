use super::spec::Clause;

#[cfg(unix)]
use super::spec::{ClauseKind, Operation, PermSpec, SymbolicClause};

#[cfg(unix)]
pub(crate) fn apply_clauses(
    clauses: &[Clause],
    mut mode: u32,
    file_type: std::fs::FileType,
) -> u32 {
    for clause in clauses {
        match &clause.kind {
            ClauseKind::Numeric(numeric) => {
                if !numeric.target.matches(file_type) {
                    continue;
                }

                let preserved = mode & !0o7777;
                mode = preserved | u32::from(numeric.mode & 0o7777);
            }
            ClauseKind::Symbolic(symbolic) => {
                if !symbolic.target.matches(file_type) {
                    continue;
                }

                mode = apply_symbolic_clause(mode, symbolic, file_type.is_dir());
            }
        }
    }

    mode
}

#[cfg(not(unix))]
#[allow(dead_code)]
pub(crate) fn apply_clauses(_clauses: &[Clause], mode: u32, _file_type: std::fs::FileType) -> u32 {
    mode
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

    mode = apply_special_bits(mode, clause);
    mode
}

#[cfg(unix)]
fn apply_special_bits(mode: u32, clause: &SymbolicClause) -> u32 {
    let mut result = mode;

    if clause.who.includes_user() {
        result = update_special_bit(result, clause.op, clause.perms.setuid, 0o4000);
    }

    if clause.who.includes_group() {
        result = update_special_bit(result, clause.op, clause.perms.setgid, 0o2000);
    }

    if clause.who.includes_other() || clause.who.covers_all() {
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

#[cfg(unix)]
#[derive(Clone, Copy)]
enum Dest {
    User,
    Group,
    Other,
}

#[cfg(unix)]
impl Dest {
    fn includes(self, clause: &SymbolicClause) -> bool {
        match self {
            Self::User => clause.who.includes_user(),
            Self::Group => clause.who.includes_group(),
            Self::Other => clause.who.includes_other(),
        }
    }

    fn permission_mask(self) -> u32 {
        match self {
            Self::User => 0o700,
            Self::Group => 0o070,
            Self::Other => 0o007,
        }
    }

    fn shift(self) -> u8 {
        match self {
            Self::User => 6,
            Self::Group => 3,
            Self::Other => 0,
        }
    }

    fn read_mask(self) -> u32 {
        match self {
            Self::User => 0o400,
            Self::Group => 0o040,
            Self::Other => 0o004,
        }
    }

    fn write_mask(self) -> u32 {
        match self {
            Self::User => 0o200,
            Self::Group => 0o020,
            Self::Other => 0o002,
        }
    }

    fn exec_mask(self) -> u32 {
        match self {
            Self::User => 0o100,
            Self::Group => 0o010,
            Self::Other => 0o001,
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn dest_permission_masks() {
        assert_eq!(Dest::User.permission_mask(), 0o700);
        assert_eq!(Dest::Group.permission_mask(), 0o070);
        assert_eq!(Dest::Other.permission_mask(), 0o007);
    }

    #[test]
    fn dest_shifts() {
        assert_eq!(Dest::User.shift(), 6);
        assert_eq!(Dest::Group.shift(), 3);
        assert_eq!(Dest::Other.shift(), 0);
    }

    #[test]
    fn dest_read_masks() {
        assert_eq!(Dest::User.read_mask(), 0o400);
        assert_eq!(Dest::Group.read_mask(), 0o040);
        assert_eq!(Dest::Other.read_mask(), 0o004);
    }

    #[test]
    fn dest_write_masks() {
        assert_eq!(Dest::User.write_mask(), 0o200);
        assert_eq!(Dest::Group.write_mask(), 0o020);
        assert_eq!(Dest::Other.write_mask(), 0o002);
    }

    #[test]
    fn dest_exec_masks() {
        assert_eq!(Dest::User.exec_mask(), 0o100);
        assert_eq!(Dest::Group.exec_mask(), 0o010);
        assert_eq!(Dest::Other.exec_mask(), 0o001);
    }

    #[test]
    fn copy_from_user_to_group() {
        let mode = 0o750;
        let result = copy_from(Dest::User, Dest::Group, mode);
        assert_eq!(result, 0o070);
    }

    #[test]
    fn copy_from_user_to_other() {
        let mode = 0o700;
        let result = copy_from(Dest::User, Dest::Other, mode);
        assert_eq!(result, 0o007);
    }

    #[test]
    fn copy_from_group_to_user() {
        let mode = 0o070;
        let result = copy_from(Dest::Group, Dest::User, mode);
        assert_eq!(result, 0o700);
    }

    #[test]
    fn copy_from_same_dest() {
        let mode = 0o750;
        let result = copy_from(Dest::User, Dest::User, mode);
        assert_eq!(result, 0o700);
    }

    #[test]
    fn update_special_bit_add_setuid() {
        let result = update_special_bit(0o755, Operation::Add, true, 0o4000);
        assert_eq!(result, 0o4755);
    }

    #[test]
    fn update_special_bit_add_no_request() {
        let result = update_special_bit(0o755, Operation::Add, false, 0o4000);
        assert_eq!(result, 0o755);
    }

    #[test]
    fn update_special_bit_remove() {
        let result = update_special_bit(0o4755, Operation::Remove, true, 0o4000);
        assert_eq!(result, 0o755);
    }

    #[test]
    fn update_special_bit_assign_true() {
        let result = update_special_bit(0o755, Operation::Assign, true, 0o2000);
        assert_eq!(result, 0o2755);
    }

    #[test]
    fn update_special_bit_assign_false() {
        let result = update_special_bit(0o2755, Operation::Assign, false, 0o2000);
        assert_eq!(result, 0o755);
    }

    #[test]
    fn permission_bits_read_only() {
        let spec = PermSpec {
            read: true,
            write: false,
            exec: false,
            exec_if_conditional: false,
            setuid: false,
            setgid: false,
            sticky: false,
            copy_user: false,
            copy_group: false,
            copy_other: false,
        };
        assert_eq!(permission_bits(&spec, Dest::User, false, 0), 0o400);
        assert_eq!(permission_bits(&spec, Dest::Group, false, 0), 0o040);
        assert_eq!(permission_bits(&spec, Dest::Other, false, 0), 0o004);
    }

    #[test]
    fn permission_bits_rwx() {
        let spec = PermSpec {
            read: true,
            write: true,
            exec: true,
            exec_if_conditional: false,
            setuid: false,
            setgid: false,
            sticky: false,
            copy_user: false,
            copy_group: false,
            copy_other: false,
        };
        assert_eq!(permission_bits(&spec, Dest::User, false, 0), 0o700);
    }

    #[test]
    fn permission_bits_exec_conditional_on_dir() {
        let spec = PermSpec {
            read: false,
            write: false,
            exec: false,
            exec_if_conditional: true,
            setuid: false,
            setgid: false,
            sticky: false,
            copy_user: false,
            copy_group: false,
            copy_other: false,
        };
        assert_eq!(permission_bits(&spec, Dest::User, true, 0), 0o100);
    }

    #[test]
    fn permission_bits_exec_conditional_on_executable_file() {
        let spec = PermSpec {
            read: false,
            write: false,
            exec: false,
            exec_if_conditional: true,
            setuid: false,
            setgid: false,
            sticky: false,
            copy_user: false,
            copy_group: false,
            copy_other: false,
        };
        assert_eq!(permission_bits(&spec, Dest::User, false, 0o111), 0o100);
    }

    #[test]
    fn permission_bits_exec_conditional_on_nonexecutable_file() {
        let spec = PermSpec {
            read: false,
            write: false,
            exec: false,
            exec_if_conditional: true,
            setuid: false,
            setgid: false,
            sticky: false,
            copy_user: false,
            copy_group: false,
            copy_other: false,
        };
        assert_eq!(permission_bits(&spec, Dest::User, false, 0o644), 0);
    }
}
