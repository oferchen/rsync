//! Process CPU and I/O priority (`--nice` / `--no-nice`).
//!
//! Mirrors upstream `niceness.c` (added in rsync 3.5.2): parses `[TARGET:]SPEC[,...]`
//! priority specifications, tracks independent local and remote settings, formats
//! the `--nice=local:...` option forwarded to a remote peer, validates daemon
//! client requests, and applies CPU nice via `platform::priority::set_cpu_priority`
//! and I/O priority via `platform::priority::set_io_priority`.

use std::fmt;
use std::io;

use logging::{debug_log, warn_log};

/// Default CPU nice level applied when a target-only `--nice` token is used.
pub const DEFAULT_NICE: i32 = 19;

/// Process role used in `--debug=CMD1` and warning messages, matching
/// upstream `niceness.c:process_role()`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessRole {
    /// Local client process.
    Client,
    /// Remote-shell or local-child `--server` process.
    Server,
    /// Daemon (`--daemon`) process.
    Daemon,
}

impl fmt::Display for ProcessRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client => f.write_str("client"),
            Self::Server => f.write_str("server"),
            Self::Daemon => f.write_str("daemon"),
        }
    }
}

/// I/O scheduling priority class, matching upstream `enum io_priority_class`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoPriorityClass {
    /// `none` (reset to kernel default).
    None,
    /// `rt_N` (`N` in `0..=7`).
    Realtime(u8),
    /// `be_N` (`N` in `0..=7`).
    BestEffort(u8),
    /// `idle`.
    Idle,
}

impl IoPriorityClass {
    fn parse(token: &str) -> Option<Self> {
        if token.eq_ignore_ascii_case("none") {
            Some(Self::None)
        } else if token.eq_ignore_ascii_case("idle") {
            Some(Self::Idle)
        } else if token.len() == 4 {
            let bytes = token.as_bytes();
            if bytes[2] == b'_' && (b'0'..=b'7').contains(&bytes[3]) {
                let data = bytes[3] - b'0';
                if bytes[..2].eq_ignore_ascii_case(b"rt") {
                    return Some(Self::Realtime(data));
                }
                if bytes[..2].eq_ignore_ascii_case(b"be") {
                    return Some(Self::BestEffort(data));
                }
            }
            None
        } else {
            None
        }
    }

    fn kernel_class_and_data(self) -> (u8, u8) {
        match self {
            Self::None => (platform::priority::IOPRIO_CLASS_NONE, 0),
            Self::Realtime(d) => (platform::priority::IOPRIO_CLASS_RT, d),
            Self::BestEffort(d) => (platform::priority::IOPRIO_CLASS_BE, d),
            Self::Idle => (platform::priority::IOPRIO_CLASS_IDLE, 0),
        }
    }
}

impl fmt::Display for IoPriorityClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Realtime(d) => write!(f, "rt_{d}"),
            Self::BestEffort(d) => write!(f, "be_{d}"),
            Self::Idle => f.write_str("idle"),
        }
    }
}

/// Priority setting for a single side (`local` or `remote`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PrioritySetting {
    /// CPU nice value (`-20..=19`), if configured.
    pub cpu: Option<i32>,
    /// I/O scheduling class and priority level, if configured.
    pub io: Option<IoPriorityClass>,
}

impl PrioritySetting {
    const fn default_nice() -> Self {
        Self {
            cpu: Some(DEFAULT_NICE),
            io: Some(IoPriorityClass::Idle),
        }
    }

    const fn is_empty(&self) -> bool {
        self.cpu.is_none() && self.io.is_none()
    }

    fn merge_from(&mut self, src: Self) {
        if let Some(cpu) = src.cpu {
            self.cpu = Some(cpu);
        }
        if let Some(io) = src.io {
            self.io = Some(io);
        }
    }
}

/// Combined local and remote priority configuration (`--nice` / `--no-nice`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PriorityOptions {
    /// Priority applied to this process.
    pub local: PrioritySetting,
    /// Priority forwarded to the remote peer (or local `--server` child).
    pub remote: PrioritySetting,
}

/// Error returned when a `--nice` / `niceness` specification cannot be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsePriorityError;

impl core::fmt::Display for ParsePriorityError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("invalid priority specification")
    }
}

impl std::error::Error for ParsePriorityError {}

impl PriorityOptions {
    /// Clears both local and remote priority settings (`--no-nice`).
    pub fn reset(&mut self) {
        self.local = PrioritySetting::default();
        self.remote = PrioritySetting::default();
    }

    /// Returns `true` if a remote priority setting is configured.
    #[must_use]
    pub const fn has_remote(&self) -> bool {
        !self.remote.is_empty()
    }

    /// Parses a `--nice` argument (`[TARGET:]SPEC[,...]`) and merges it into
    /// `self` atomically (leaving `self` unchanged on error).
    pub fn parse_into(&mut self, arg: &str) -> Result<(), ParsePriorityError> {
        if arg.is_empty() {
            return Err(ParsePriorityError);
        }
        let mut parsed = *self;
        for token in arg.split(',') {
            if token.is_empty() {
                return Err(ParsePriorityError);
            }
            let (local, remote, setting) = if let Some((target, spec)) = token.split_once(':') {
                let (l, r) = parse_target(target).ok_or(ParsePriorityError)?;
                let s = parse_priority(spec).ok_or(ParsePriorityError)?;
                (l, r, s)
            } else if let Some((l, r)) = parse_target(token) {
                (l, r, PrioritySetting::default_nice())
            } else {
                let s = parse_priority(token).ok_or(ParsePriorityError)?;
                (true, true, s)
            };
            if local {
                parsed.local.merge_from(setting);
            }
            if remote {
                parsed.remote.merge_from(setting);
            }
        }
        *self = parsed;
        Ok(())
    }

    /// Formats the `--nice=local:...` argument to forward to a remote peer,
    /// mirroring upstream `niceness_remote_option()`.
    #[must_use]
    pub fn remote_option(&self) -> Option<String> {
        let s = self.remote;
        match (s.cpu, s.io) {
            (None, None) => None,
            (Some(cpu), Some(io)) => Some(format!("--nice=local:{cpu}/{io}")),
            (Some(cpu), None) => Some(format!("--nice=local:{cpu}")),
            (None, Some(io)) => Some(format!("--nice=local:{io}")),
        }
    }

    /// Validates a daemon client's requested niceness, matching upstream
    /// `niceness_check_daemon_request()`.
    pub fn check_daemon_request(&self) -> Result<(), &'static str> {
        if let Some(cpu) = self.local.cpu
            && cpu != DEFAULT_NICE
        {
            return Err("daemon clients may only request CPU nice 19");
        }
        if let Some(io) = self.local.io
            && io != IoPriorityClass::Idle
        {
            return Err("daemon clients may only request idle I/O priority");
        }
        Ok(())
    }

    /// Applies and clears `self.local` for `role`, mirroring upstream
    /// `niceness_apply()`.
    pub fn apply_local(&mut self, role: ProcessRole) {
        let setting = self.local;
        self.local = PrioritySetting::default();

        let mut cpu_applied = true;
        if let Some(cpu) = setting.cpu {
            cpu_applied = apply_cpu_priority(role, cpu);
        }
        if let Some(io) = setting.io {
            if role == ProcessRole::Daemon
                && setting.cpu.is_some()
                && io == IoPriorityClass::None
                && !cpu_applied
            {
                warn_log!(
                    "not resetting daemon I/O priority because the CPU priority change failed"
                );
                return;
            }
            apply_io_priority(role, io);
        }
    }
}

fn parse_target(token: &str) -> Option<(bool, bool)> {
    if token.eq_ignore_ascii_case("local") {
        Some((true, false))
    } else if token.eq_ignore_ascii_case("remote") {
        Some((false, true))
    } else if token.eq_ignore_ascii_case("all") {
        Some((true, true))
    } else {
        None
    }
}

fn parse_priority(spec: &str) -> Option<PrioritySetting> {
    if let Some(io) = IoPriorityClass::parse(spec) {
        return Some(PrioritySetting {
            cpu: None,
            io: Some(io),
        });
    }
    let (cpu_str, io_str) = match spec.split_once('/') {
        Some((c, i)) if !i.is_empty() => (c, Some(i)),
        Some(_) => return None,
        None => (spec, None),
    };
    let bytes = cpu_str.as_bytes();
    let valid_start = match bytes.first() {
        Some(b) if b.is_ascii_digit() => true,
        Some(b'+' | b'-') => bytes.get(1).is_some_and(u8::is_ascii_digit),
        _ => false,
    };
    if !valid_start {
        return None;
    }
    let cpu: i32 = cpu_str.parse().ok()?;
    if !(-20..=19).contains(&cpu) {
        return None;
    }
    let io = match io_str {
        Some(s) => Some(IoPriorityClass::parse(s)?),
        None => None,
    };
    Some(PrioritySetting { cpu: Some(cpu), io })
}

fn apply_cpu_priority(role: ProcessRole, priority: i32) -> bool {
    match platform::priority::set_cpu_priority(priority) {
        Ok(()) => {
            debug_log!(Cmd, 1, "set {role} CPU priority to {priority}");
            true
        }
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            warn_log!(
                "cannot set {role} CPU priority to {priority}: operation is not supported on this system"
            );
            false
        }
        Err(err) => {
            warn_log!("failed to set {role} CPU priority to {priority}: {err}");
            false
        }
    }
}

fn apply_io_priority(role: ProcessRole, io_class: IoPriorityClass) {
    let (kernel_class, data) = io_class.kernel_class_and_data();
    match platform::priority::set_io_priority(kernel_class, data) {
        Ok(()) => {
            debug_log!(Cmd, 1, "set {role} I/O priority to {io_class}");
        }
        Err(err) if err.kind() == io::ErrorKind::Unsupported => {
            warn_log!(
                "cannot set {role} I/O priority to {io_class}: operation is not supported on this system"
            );
        }
        Err(err) => {
            warn_log!("failed to set {role} I/O priority to {io_class}: {err}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_specs() {
        let mut opts = PriorityOptions::default();
        opts.parse_into("remote:5/be_6").unwrap();
        assert_eq!(opts.remote_option().as_deref(), Some("--nice=local:5/be_6"));
        assert!(opts.local.is_empty());

        let mut opts = PriorityOptions::default();
        opts.parse_into("ALL").unwrap();
        assert_eq!(
            opts.remote_option().as_deref(),
            Some("--nice=local:19/idle")
        );
        assert_eq!(opts.local.cpu, Some(19));
        assert_eq!(opts.local.io, Some(IoPriorityClass::Idle));
    }

    #[test]
    fn rejects_invalid_specs() {
        for bad in [
            "",
            "local:",
            "2/",
            "20",
            "+20",
            "-21",
            "999999999999999999999999",
            "be_8",
            "rt_9",
            "locality",
            " 1",
            "	1",
            " local:1",
            "local: 1",
            "local::1",
            "idle/none",
            "1//idle",
            "none/idle",
            "local:1,",
            "local:1,,remote:2",
            "somewhere:1",
            "19/idle/x",
        ] {
            let mut opts = PriorityOptions::default();
            assert!(opts.parse_into(bad).is_err(), "expected err for {bad:?}");
        }
    }
}
