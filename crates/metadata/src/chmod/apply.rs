//! Evaluator that applies parsed [`Clause`] modifiers to a mode value.
//!
//! Implements both numeric and symbolic chmod semantics matching GNU `chmod`,
//! including conditional-exec (`X`), copy directives (e.g. `g=u`), and the
//! upstream-defined target selectors (`F`, `D`).

use super::spec::Clause;

#[cfg(unix)]
use super::spec::{ClauseKind, CopySource, Operation, PermSpec, SymbolicClause};

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
#[allow(dead_code)] // REASON: used on unix; stub on other platforms
pub(crate) fn apply_clauses(_clauses: &[Clause], mode: u32, _file_type: std::fs::FileType) -> u32 {
    mode
}

/// Returns the process umask, cached for thread safety.
///
/// upstream: `main.c` stores `orig_umask` once at startup. We query it
/// the first time a chmod clause with an implied who-specifier is applied
/// and cache the result so the double set-and-restore syscall happens at
/// most once per process.
#[cfg(unix)]
#[allow(unsafe_code)]
fn cached_umask() -> u32 {
    use std::sync::OnceLock;
    static UMASK: OnceLock<u32> = OnceLock::new();
    *UMASK.get_or_init(|| {
        // SAFETY: umask is a standard POSIX call. We set it to 0 to read
        // the current value, then immediately restore it. This is a
        // well-known pattern (used by upstream rsync main.c, GNU coreutils,
        // etc.). The OnceLock ensures this pair of calls happens at most
        // once per process, eliminating any window for concurrent umask
        // modifications.
        let old = unsafe { libc::umask(0) };
        unsafe { libc::umask(old) };
        old as u32
    })
}

/// Extracts a copy source's three rwx permission bits, normalised to the low
/// three bits (`0b_rwx`).
///
/// upstream: chmod.c mode_copy_bits() shifts the selected category down by 6
/// (user), 3 (group), or 0 (other) and masks with `7`.
#[cfg(unix)]
const fn copy_source_bits(mode: u32, src: CopySource) -> u32 {
    let shifted = match src {
        CopySource::User => mode >> 6,
        CopySource::Group => mode >> 3,
        CopySource::Other => mode,
    };
    shifted & 0o7
}

#[cfg(unix)]
fn apply_symbolic_clause(mut mode: u32, clause: &SymbolicClause, is_dir: bool) -> u32 {
    let before = mode;

    // upstream: chmod.c - when no who-specifier is given, the computed
    // permission bits are masked by ~orig_umask. This prevents `+w` from
    // granting world-writable when the umask forbids it.
    let umask_mask = if clause.who_implied {
        !cached_umask() & 0o777
    } else {
        0o777
    };

    // upstream: chmod.c mode_copy_bits() - a copy directive (`g=u`) reads the
    // source category's rwx bits from the *original* mode and replicates them
    // into every destination who-class. The source bits are sampled once,
    // before any destination is mutated.
    let copy_src_bits = clause
        .perms
        .copy_source
        .map(|src| copy_source_bits(before, src));

    for dest in [Dest::User, Dest::Group, Dest::Other] {
        if !dest.includes(clause) {
            continue;
        }

        let mask = dest.permission_mask();
        let mut bits = mode & mask;

        if matches!(clause.op, Operation::Assign) {
            bits = 0;
        }

        let add_bits = match copy_src_bits {
            Some(src_bits) => (dest.spread_exec(src_bits) & mask) & umask_mask,
            None => permission_bits(&clause.perms, dest, is_dir, before) & umask_mask,
        };
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
const fn apply_special_bits(mode: u32, clause: &SymbolicClause) -> u32 {
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
const fn update_special_bit(current: u32, op: Operation, flag_requested: bool, bit: u32) -> u32 {
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
    const fn includes(self, clause: &SymbolicClause) -> bool {
        match self {
            Self::User => clause.who.includes_user(),
            Self::Group => clause.who.includes_group(),
            Self::Other => clause.who.includes_other(),
        }
    }

    const fn permission_mask(self) -> u32 {
        match self {
            Self::User => 0o700,
            Self::Group => 0o070,
            Self::Other => 0o007,
        }
    }

    const fn read_mask(self) -> u32 {
        match self {
            Self::User => 0o400,
            Self::Group => 0o040,
            Self::Other => 0o004,
        }
    }

    const fn write_mask(self) -> u32 {
        match self {
            Self::User => 0o200,
            Self::Group => 0o020,
            Self::Other => 0o002,
        }
    }

    const fn exec_mask(self) -> u32 {
        match self {
            Self::User => 0o100,
            Self::Group => 0o010,
            Self::Other => 0o001,
        }
    }

    /// Shifts a normalised low-three-bit rwx value (`0b_rwx`) into this
    /// destination's permission triad. upstream: chmod.c mode_copy_bits()
    /// multiplies `copy_bits` by `copy_dst` (0100/0010/0001).
    const fn spread_exec(self, low_bits: u32) -> u32 {
        match self {
            Self::User => low_bits << 6,
            Self::Group => low_bits << 3,
            Self::Other => low_bits,
        }
    }
}

#[cfg(unix)]
const fn permission_bits(spec: &PermSpec, dest: Dest, is_dir: bool, before: u32) -> u32 {
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
            copy_source: None,
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
            copy_source: None,
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
            copy_source: None,
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
            copy_source: None,
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
            copy_source: None,
        };
        assert_eq!(permission_bits(&spec, Dest::User, false, 0o644), 0);
    }
}
