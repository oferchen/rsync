/// Path to the kernel's active-LSM list pseudofile.
///
/// On a Linux system with securityfs mounted, `/sys/kernel/security/lsm`
/// contains a comma-separated list of the LSMs active in the kernel (for
/// example, `lockdown,capability,landlock,yama,apparmor,bpf`). The file is
/// absent when securityfs is not mounted - typical for stripped-down
/// containers without `/sys` exposed.
#[cfg(target_os = "linux")]
const LSM_LIST_PATH: &str = "/sys/kernel/security/lsm";

/// Applies the Linux-only startup defense-in-depth hardenings.
///
/// Runs two surgical changes once at daemon main-entry, before the listener
/// is bound or any privileged hook is exec'd:
///
/// 1. Sets `PR_SET_NO_NEW_PRIVS` so any later `execve()` (notably the
///    pre/post-xfer-exec hook helpers) cannot acquire elevated privileges
///    via setuid binaries. This is also required for proper Landlock
///    engagement on kernels that gate the LSM behind the no-new-privs bit.
/// 2. Reads `/sys/kernel/security/lsm` and emits the active LSM list to
///    the daemon log so operators can confirm at a glance which kernel
///    security modules cover the process - in particular whether Landlock
///    is loaded alongside SEC-1 *at* helpers.
///
/// Both steps are best-effort observability and short-circuit to a no-op
/// on non-Linux targets. Failures inside either step never propagate: the
/// daemon still serves connections, but the operator sees a diagnostic in
/// the log explaining the degraded posture.
fn apply_startup_hardening(log_sink: Option<&SharedLogSink>) {
    apply_no_new_privs(log_sink);
    log_active_lsms(log_sink);
}

/// Sets `PR_SET_NO_NEW_PRIVS` for the daemon process.
///
/// On Linux, calls `prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)` via the safe
/// `nix::sys::prctl::set_no_new_privs` wrapper. The flag is a one-way bit:
/// once set it cannot be cleared, and it propagates to every child of the
/// process. After this call any `execve()` of a setuid or setgid binary
/// (or one with capabilities or a Linux Security Module that grants extra
/// privileges via the exec) is silently downgraded to the calling user's
/// privileges.
///
/// On non-Linux targets this is a no-op.
#[cfg(target_os = "linux")]
fn apply_no_new_privs(log_sink: Option<&SharedLogSink>) {
    match nix::sys::prctl::set_no_new_privs() {
        Ok(()) => {
            if let Some(log) = log_sink {
                let message = rsync_info!("PR_SET_NO_NEW_PRIVS active for daemon process")
                    .with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
        Err(err) => {
            if let Some(log) = log_sink {
                let text = format!(
                    "PR_SET_NO_NEW_PRIVS prctl failed: {err}; continuing without it"
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
}

/// Stub for platforms without `prctl(2)`.
#[cfg(not(target_os = "linux"))]
fn apply_no_new_privs(_log_sink: Option<&SharedLogSink>) {}

/// Reads `/sys/kernel/security/lsm` and logs the active LSM list.
///
/// The pseudofile is exposed by securityfs and lists the comma-separated
/// names of the Linux Security Modules currently loaded in the kernel.
/// Logging this once at startup gives operators a single audit line that
/// confirms which kernel-side defenses (Landlock, AppArmor, SELinux, Yama,
/// lockdown) cover the daemon process.
///
/// A missing file or read error is logged at debug level - LSM detection
/// is purely observability, so the absence of securityfs (common in
/// minimal containers) must not block daemon startup.
///
/// On non-Linux targets this is a no-op.
#[cfg(target_os = "linux")]
fn log_active_lsms(log_sink: Option<&SharedLogSink>) {
    read_and_log_lsms_from(Path::new(LSM_LIST_PATH), log_sink);
}

/// Stub for platforms without securityfs.
#[cfg(not(target_os = "linux"))]
fn log_active_lsms(_log_sink: Option<&SharedLogSink>) {}

/// Reads the LSM list from `path` and logs it, factored for testing.
///
/// Trims whitespace and emits an info diagnostic on success. On error
/// (file missing, unreadable, or empty after trim) emits a debug
/// diagnostic so the operator can confirm the read was attempted without
/// promoting expected absences (rootless containers) to warning noise.
#[cfg(target_os = "linux")]
fn read_and_log_lsms_from(path: &Path, log_sink: Option<&SharedLogSink>) {
    match fs::read_to_string(path) {
        Ok(contents) => {
            let lsms = contents.trim();
            if lsms.is_empty() {
                if let Some(log) = log_sink {
                    let text = format!(
                        "active Linux Security Modules: <empty> (read from {})",
                        path.display(),
                    );
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return;
            }

            if let Some(log) = log_sink {
                let text = format!("active Linux Security Modules: {lsms}");
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
        Err(err) => {
            if let Some(log) = log_sink {
                let text = format!(
                    "active Linux Security Modules: detection skipped ({} unavailable: {err})",
                    path.display(),
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod hardening_tests {
    use super::*;
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};

    /// Builds a [`SharedLogSink`] backed by a fresh tempfile and returns
    /// the sink alongside a clone of the underlying file. The clone is
    /// used to read back the rendered log output without taking the sink
    /// lock from the test thread.
    fn capturing_sink() -> (SharedLogSink, std::fs::File) {
        let file = tempfile::tempfile().expect("create capturing tempfile");
        let reader = file.try_clone().expect("clone capturing tempfile");
        let sink = Arc::new(Mutex::new(MessageSink::with_brand(file, Brand::Oc)));
        (sink, reader)
    }

    /// Reads the captured log file end-to-end and returns the rendered text.
    fn drain_capture(mut reader: std::fs::File) -> String {
        reader
            .seek(SeekFrom::Start(0))
            .expect("rewind capturing tempfile");
        let mut buf = String::new();
        reader
            .read_to_string(&mut buf)
            .expect("read capturing tempfile");
        buf
    }

    /// A valid LSM list parses, trims, and is forwarded to the log sink
    /// verbatim. The mocked file uses the kernel's documented format -
    /// comma-separated names with a trailing newline - so the test
    /// exercises the exact trim contract operators see in production.
    #[test]
    fn read_and_log_lsms_emits_trimmed_list() {
        let dir = tempfile::tempdir().expect("create tempdir for LSM mock");
        let path = dir.path().join("lsm");
        let mut file = std::fs::File::create(&path).expect("create LSM mock file");
        file.write_all(b"lockdown,capability,landlock,yama,bpf\n")
            .expect("write LSM mock contents");
        file.flush().expect("flush LSM mock contents");

        let (sink, reader) = capturing_sink();
        read_and_log_lsms_from(&path, Some(&sink));
        let captured = drain_capture(reader);

        assert!(
            captured.contains(
                "active Linux Security Modules: lockdown,capability,landlock,yama,bpf"
            ),
            "expected trimmed LSM list in log output, got: {captured:?}",
        );
    }

    /// A missing securityfs pseudofile must not panic and must not
    /// promote the read failure to an error. The graceful-skip path
    /// keeps daemon startup quiet on minimal containers.
    #[test]
    fn read_and_log_lsms_skips_when_file_missing() {
        let dir = tempfile::tempdir().expect("create tempdir for missing-path probe");
        let path = dir.path().join("does-not-exist");

        let (sink, reader) = capturing_sink();
        read_and_log_lsms_from(&path, Some(&sink));
        let captured = drain_capture(reader);

        assert!(
            captured.contains("detection skipped") && captured.contains("unavailable"),
            "expected skip diagnostic, got: {captured:?}",
        );
    }
}
