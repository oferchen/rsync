#![deny(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::io::Write;

use core::branding::Brand;
use core::message::Role;
use core::rsync_error;
use logging::MessageSink;

const SERVER_UNAVAILABLE_MESSAGE: &str = "native --server mode is not yet implemented; oc-rsync no longer delegates to system rsync binaries";

/// Returns the daemon argument vector when `--daemon` is present.
pub(crate) fn daemon_mode_arguments(args: &[OsString]) -> Option<Vec<OsString>> {
    if args.is_empty() {
        return None;
    }

    let program_name = super::detect_program_name(args.first().map(OsString::as_os_str));
    let daemon_program = match program_name {
        super::ProgramName::Rsync => Brand::Upstream.daemon_program_name(),
        super::ProgramName::OcRsync => Brand::Oc.daemon_program_name(),
    };

    let mut daemon_args = Vec::with_capacity(args.len());
    daemon_args.push(OsString::from(daemon_program));

    let mut found = false;
    let mut reached_double_dash = false;

    for arg in args.iter().skip(1) {
        if !reached_double_dash && arg == "--" {
            reached_double_dash = true;
            daemon_args.push(arg.clone());
            continue;
        }

        if !reached_double_dash && arg == "--daemon" {
            found = true;
            continue;
        }

        daemon_args.push(arg.clone());
    }

    if found { Some(daemon_args) } else { None }
}

/// Returns `true` when the invocation requests server mode.
pub(crate) fn server_mode_requested(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|arg| arg == "--server")
}

/// Delegates execution to the daemon front-end (Unix) or reports that daemon
/// mode is unavailable (Windows).
#[cfg(unix)]
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    // On Unix, delegate to the actual daemon front-end.
    daemon::run(args, stdout, stderr)
}

#[cfg(windows)]
pub(crate) fn run_daemon_mode<Out, Err>(
    args: Vec<OsString>,
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();

    write_daemon_unavailable_error(stderr, program_brand);
    1
}

/// Reports that native `--server` handling is unavailable.
pub(crate) fn run_server_mode<Out, Err>(
    args: &[OsString],
    stdout: &mut Out,
    stderr: &mut Err,
) -> i32
where
    Out: Write,
    Err: Write,
{
    let _ = stdout.flush();
    let _ = stderr.flush();

    let program_brand = super::detect_program_name(args.first().map(OsString::as_os_str)).brand();
    write_server_unavailable_error(stderr, program_brand, SERVER_UNAVAILABLE_MESSAGE);
    1
}

fn write_server_unavailable_error<Err: Write>(
    stderr: &mut Err,
    brand: Brand,
    text: impl fmt::Display,
) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(1, "{}", text);
    message = message.with_role(Role::Server);
    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(sink.writer_mut(), "{text}");
    }
}

#[cfg(windows)]
fn write_daemon_unavailable_error<Err: Write>(stderr: &mut Err, brand: Brand) {
    let mut sink = MessageSink::with_brand(stderr, brand);
    let mut message = rsync_error!(
        1,
        "daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"
    );
    message = message.with_role(Role::Client);

    if super::write_message(&message, &mut sink).is_err() {
        let _ = writeln!(
            sink.writer_mut(),
            "daemon mode is not supported on this platform; run the oc-rsync daemon on a Unix-like system"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::env;
    use std::ffi::{OsStr, OsString};
    use std::sync::Mutex;

    use core::branding::{client_program_name, rust_version};
    use core::fallback::CLIENT_FALLBACK_ENV;

    const RSYNC: &str = client_program_name();
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn server_mode_reports_unavailability() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = super::super::run(
            [
                OsString::from(RSYNC),
                OsString::from("--server"),
                OsString::from("--sender"),
                OsString::from("."),
                OsString::from("dest"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        assert!(stdout.is_empty());
        let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
        assert!(
            stderr_text.contains(SERVER_UNAVAILABLE_MESSAGE),
            "stderr should mention unavailable server mode"
        );
        assert_contains_server_trailer(&stderr_text);
    }

    #[cfg(unix)]
    #[test]
    fn server_mode_ignores_fallback_overrides() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let _env_lock = ENV_LOCK.lock().expect("env lock");

        let temp = tempdir().expect("tempdir");
        let script_path = temp.path().join("server.sh");
        let marker_path = temp.path().join("marker.txt");

        fs::write(
            &script_path,
            b"#!/bin/sh\nset -eu\nprintf 'invoked' > \"$SERVER_MARKER\"\n",
        )
        .expect("write script");

        let mut perms = fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("set script perms");

        let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
        let _marker_guard = EnvGuard::set("SERVER_MARKER", marker_path.as_os_str());

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit_code = super::super::run(
            [
                OsString::from(RSYNC),
                OsString::from("--server"),
                OsString::from("--sender"),
                OsString::from("."),
                OsString::from("dest"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit_code, 1);
        assert!(!marker_path.exists(), "fallback script should not run");
        assert!(stdout.is_empty());
        let stderr_text = String::from_utf8(stderr).expect("stderr utf8");
        assert!(stderr_text.contains(SERVER_UNAVAILABLE_MESSAGE));
        assert_contains_server_trailer(&stderr_text);
    }

    fn assert_contains_server_trailer(rendered: &str) {
        let expected = format!("[server={}]", rust_version());
        assert!(
            rendered.contains(&expected),
            "expected message to contain {expected:?}, got {rendered:?}"
        );
    }

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &OsStr) -> Self {
            let previous = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                unsafe {
                    env::set_var(self.key, previous);
                }
            } else {
                unsafe {
                    env::remove_var(self.key);
                }
            }
        }
    }
}
