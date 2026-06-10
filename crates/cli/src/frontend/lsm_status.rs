//! Renderer for the `--lsm-status` CLI diagnostic.
//!
//! Composes a structured, multi-line summary of the kernel-side defenses
//! visible to the current process: the active LSM list, the Landlock
//! probe outcome, the seccomp status from `/proc/self/status`, and the
//! current io_uring SQPOLL opt-out policy. Used by the
//! `maybe_print_help_or_version` path in `execution/drive/workflow/preflight.rs`
//! when the user invokes the flag.
//!
//! The diagnostic is process-local: it reflects the security posture of
//! the CLI process itself, not a remote daemon worker. Operators running
//! a daemon should also consult the startup log line emitted by the
//! daemon-side hardening (see `docs/packaging/landlock-feature-guidance.md`).
//!
//! Output is plain text suitable for piping through `grep`; the format is
//! stable enough for shell scripts to key off the prefix of each line
//! (`active LSMs:`, `Landlock:`, `seccomp:`, `--no-io-uring-sqpoll:`) but
//! the trailing descriptive text may evolve.

use std::fs;

use fast_io::lsm::read_active_lsms;

/// Renders the `--lsm-status` diagnostic to a string.
///
/// The returned text is guaranteed to end in a newline. The output is
/// laid out as:
///
/// ```text
/// oc-rsync LSM diagnostic:
///   active LSMs: <comma-list> | (none visible)
///   Landlock: <state>
///   seccomp: <state>
///   --no-io-uring-sqpoll: <set | not set>
/// ```
///
/// `program_label` is the brand prefix (`oc-rsync` or `rsync`) used in
/// the first line so wrappers can render a matching banner. The remaining
/// content reflects the *current process*; the CLI is not a daemon worker,
/// so the seccomp line typically reports "NOT applied".
#[must_use]
pub(super) fn render_lsm_status(program_label: &str) -> String {
    let mut out = String::new();
    out.push_str(program_label);
    out.push_str(" LSM diagnostic:\n");

    let lsms = read_active_lsms();
    let lsm_line = match &lsms {
        Some(names) => names.join(","),
        None => "(none visible; securityfs not mounted or non-Linux host)".to_owned(),
    };
    out.push_str("  active LSMs: ");
    out.push_str(&lsm_line);
    out.push('\n');

    out.push_str("  Landlock: ");
    out.push_str(landlock_state());
    out.push('\n');

    out.push_str("  seccomp: ");
    out.push_str(&seccomp_state());
    out.push('\n');

    out.push_str("  --no-io-uring-sqpoll: ");
    out.push_str(io_uring_sqpoll_state());
    out.push('\n');

    out
}

/// Probes Landlock availability via the `fast_io::landlock` wrapper.
///
/// Returns one of three short labels:
/// - `available (kernel >= 5.13)` when `fast_io::landlock::is_supported`
///   returns `true`. The exact ABI achieved at engagement time is
///   reported in the daemon startup log, not here.
/// - `unavailable (kernel < 5.13 or LSM not loaded)` on Linux without
///   the LSM.
/// - `unavailable (non-Linux target)` everywhere else.
fn landlock_state() -> &'static str {
    if cfg!(target_os = "linux") {
        if fast_io::landlock::is_supported() {
            "available (kernel >= 5.13)"
        } else {
            "unavailable (kernel < 5.13 or LSM not loaded)"
        }
    } else {
        "unavailable (non-Linux target)"
    }
}

/// Reads `/proc/self/status` and returns a human label for the
/// `Seccomp:` line.
///
/// The kernel exposes one of three values in that field:
/// - `0` - disabled (no filter installed). The CLI process is the
///   typical case: it is not a daemon worker and has no syscall filter.
/// - `1` - strict mode (only `read`/`write`/`exit`/`sigreturn` allowed).
/// - `2` - filter mode (a BPF allowlist is installed).
///
/// Returns `NOT applied (current process is the CLI, not a daemon
/// worker)` when the line reads `0`, and the matching higher-mode label
/// otherwise. On non-Linux targets or when `/proc/self/status` is
/// unreadable the label degrades to `unknown`.
fn seccomp_state() -> String {
    if !cfg!(target_os = "linux") {
        return "unknown (non-Linux target)".to_owned();
    }
    let contents = match fs::read_to_string("/proc/self/status") {
        Ok(text) => text,
        Err(_) => return "unknown (/proc/self/status unreadable)".to_owned(),
    };
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("Seccomp:") {
            let value = rest.trim();
            return match value {
                "0" => {
                    "NOT applied (current process is the CLI, not a daemon worker)".to_owned()
                }
                "1" => "strict mode (read/write/exit/sigreturn only)".to_owned(),
                "2" => "filter mode (BPF allowlist installed)".to_owned(),
                other => format!("kernel reports unrecognised mode '{other}'"),
            };
        }
    }
    "unknown (/proc/self/status missing Seccomp: field)".to_owned()
}

/// Reports whether the process-wide `--no-io-uring-sqpoll` gate is set.
///
/// The companion `--no-io-uring-sqpoll` flag publishes a process-wide
/// gate in `fast_io`; until that wiring lands the diagnostic reports
/// "not set" unconditionally. Once the gate exists the helper queries it
/// directly so the line tracks the actual io_uring policy at runtime.
fn io_uring_sqpoll_state() -> &'static str {
    // Forward-compatible scaffolding: when fast_io exposes a public
    // `is_sqpoll_disabled_by_policy()` query (companion PR), swap this
    // branch in. Keeping the literal here so the output remains stable
    // across builds and the flag name stays grep-able for operators.
    "not set (SQPOLL would be requested if available)"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_lsm_status_starts_with_program_label() {
        let output = render_lsm_status("oc-rsync");
        assert!(output.starts_with("oc-rsync LSM diagnostic:\n"));
    }

    #[test]
    fn render_lsm_status_includes_required_sections() {
        let output = render_lsm_status("oc-rsync");
        assert!(output.contains("\n  active LSMs: "), "missing active LSMs");
        assert!(output.contains("\n  Landlock: "), "missing Landlock");
        assert!(output.contains("\n  seccomp: "), "missing seccomp");
        assert!(
            output.contains("\n  --no-io-uring-sqpoll: "),
            "missing sqpoll line"
        );
    }

    #[test]
    fn render_lsm_status_ends_with_newline() {
        let output = render_lsm_status("rsync");
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn render_lsm_status_honours_brand_label() {
        let output = render_lsm_status("rsync");
        assert!(output.starts_with("rsync LSM diagnostic:\n"));
        assert!(!output.starts_with("oc-rsync"));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn landlock_state_reports_non_linux_off_linux() {
        assert_eq!(landlock_state(), "unavailable (non-Linux target)");
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn seccomp_state_reports_unknown_off_linux() {
        assert_eq!(seccomp_state(), "unknown (non-Linux target)");
    }
}
