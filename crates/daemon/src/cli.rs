use std::env;
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};

use rsync_core::{
    branding::Brand,
    fallback::{
        CLIENT_FALLBACK_ENV, DAEMON_AUTO_DELEGATE_ENV, DAEMON_FALLBACK_ENV, FallbackOverride,
        describe_missing_fallback_binary, fallback_binary_available, fallback_override,
    },
    message::Role,
    rsync_error,
    version::VersionInfoReport,
};
use rsync_logging::MessageSink;

use crate::{
    config::DaemonConfig,
    daemon::{
        MAX_EXIT_CODE, ParsedArgs, configured_fallback_binary, parse_args, render_help, run_daemon,
        write_message,
    },
};

/// Runs the daemon CLI using the provided argument iterator and output handles.
///
/// The function returns the process exit code that should be used by the caller.
/// Diagnostics are rendered using the central [`rsync_core::message`] utilities.
#[allow(clippy::module_name_repetitions)]
pub fn run<I, S, Out, Err>(arguments: I, stdout: &mut Out, stderr: &mut Err) -> i32
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
{
    let mut stderr_sink = MessageSink::new(stderr);
    match parse_args(arguments) {
        Ok(parsed) => execute(parsed, stdout, &mut stderr_sink),
        Err(error) => {
            let mut message = rsync_error!(1, "{}", error).with_role(Role::Daemon);
            if write_message(&message, &mut stderr_sink).is_err() {
                let _ = writeln!(stderr_sink.writer_mut(), "{error}");
            }
            1
        }
    }
}

fn execute<Out, Err>(parsed: ParsedArgs, stdout: &mut Out, stderr: &mut MessageSink<Err>) -> i32
where
    Out: Write,
    Err: Write,
{
    // 1) handle help/version fast-paths
    if parsed.show_help {
        let help = render_help(parsed.program_name);
        if stdout.write_all(help.as_bytes()).is_err() {
            let _ = writeln!(stdout, "{help}");
            return 1;
        }
        return 0;
    }

    if parsed.show_version && parsed.remainder.is_empty() {
        let report = VersionInfoReport::for_daemon_brand(parsed.program_name.brand());
        let banner = report.human_readable();
        if stdout.write_all(banner.as_bytes()).is_err() {
            return 1;
        }
        return 0;
    }

    // 2) decide if we should delegate
    //
    // CI supplies: --config <file> --daemon --no-detach --log-file <file>
    // AND sets OC_RSYNC_DAEMON_FALLBACK. Older upstreams can crash. If we see
    // a real --config, prefer native Rust daemon over auto-delegate.
    let has_explicit_config = remainder_has_config(&parsed.remainder);

    if parsed.delegate_system_rsync {
        // explicit user request still wins
        return run_delegate_mode(parsed.remainder.as_slice(), stderr);
    }

    if !has_explicit_config
        && (auto_delegate_system_rsync_enabled() || fallback_binary_configured())
    {
        // only env-based / auto delegation when we don't see a concrete config
        return run_delegate_mode(parsed.remainder.as_slice(), stderr);
    }

    // 3) run native daemon mode
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .brand(parsed.program_name.brand())
        .arguments(parsed.remainder)
        .build();

    match run_daemon(config) {
        Ok(()) => 0,
        Err(error) => {
            if write_message(error.message(), stderr).is_err() {
                let message = error.message();
                let _ = writeln!(stderr.writer_mut(), "{message}");
            }
            error.exit_code()
        }
    }
}

/// scan the remainder of the CLI args to see if a concrete config file was requested
fn remainder_has_config(args: &[OsString]) -> bool {
    for arg in args {
        // exact: --config <file>
        if arg == "--config" {
            return true;
        }
        // inline: --config=/path/to/file
        if let Some(s) = arg.to_str() {
            if let Some(rest) = s.strip_prefix("--config=") {
                if !rest.trim().is_empty() {
                    return true;
                }
            }
        }
    }
    false
}

fn auto_delegate_system_rsync_enabled() -> bool {
    matches!(env_flag(DAEMON_AUTO_DELEGATE_ENV), Some(true))
}

pub(super) fn fallback_binary_configured() -> bool {
    if override_disables_fallback(DAEMON_FALLBACK_ENV)
        || override_disables_fallback(CLIENT_FALLBACK_ENV)
    {
        return false;
    }

    configured_fallback_binary()
        .map(|binary| fallback_binary_available(binary.as_os_str()))
        .unwrap_or(false)
}

fn override_disables_fallback(name: &str) -> bool {
    matches!(fallback_override(name), Some(FallbackOverride::Disabled))
}

fn fallback_binary() -> OsString {
    configured_fallback_binary()
        .unwrap_or_else(|| OsString::from(Brand::Upstream.client_program_name()))
}

fn env_flag(name: &str) -> Option<bool> {
    let value = env::var_os(name)?;
    let value = value.to_string_lossy();
    let trimmed = value.trim();

    if trimmed.is_empty() {
        return Some(true);
    }

    if trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("off")
    {
        Some(false)
    } else {
        Some(true)
    }
}

/// Converts a numeric exit code into an [`std::process::ExitCode`].
#[must_use]
pub fn exit_code_from(status: i32) -> std::process::ExitCode {
    let clamped = status.clamp(0, MAX_EXIT_CODE);
    std::process::ExitCode::from(clamped as u8)
}

fn run_delegate_mode<Err>(args: &[OsString], stderr: &mut MessageSink<Err>) -> i32
where
    Err: Write,
{
    let binary = fallback_binary();

    if !fallback_binary_available(binary.as_os_str()) {
        let diagnostic = describe_missing_fallback_binary(
            binary.as_os_str(),
            &[DAEMON_FALLBACK_ENV, CLIENT_FALLBACK_ENV],
        );
        let message = rsync_error!(1, diagnostic).with_role(Role::Daemon);
        let fallback = message.to_string();
        if write_message(&message, stderr).is_err() {
            let _ = writeln!(stderr.writer_mut(), "{fallback}");
        }
        return 1;
    }

    let mut command = ProcessCommand::new(&binary);
    command.arg("--daemon");
    command.arg("--no-detach");
    command.args(args);
    command.stdin(Stdio::inherit());
    command.stdout(Stdio::inherit());
    command.stderr(Stdio::inherit());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let binary_display = Path::new(&binary).display();
            let message = rsync_error!(
                1,
                format!("failed to launch system rsync daemon '{binary_display}': {error}")
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to launch system rsync daemon '{binary_display}': {error}"
                );
            }
            return 1;
        }
    };

    match child.wait() {
        Ok(status) => {
            if status.success() {
                0
            } else {
                let code = status.code().unwrap_or(MAX_EXIT_CODE);
                let message = rsync_error!(
                    code,
                    format!("system rsync daemon exited with status {status}")
                )
                .with_role(Role::Daemon);
                if write_message(&message, stderr).is_err() {
                    let _ = writeln!(
                        stderr.writer_mut(),
                        "system rsync daemon exited with status {status}"
                    );
                }
                code
            }
        }
        Err(error) => {
            let message = rsync_error!(
                1,
                format!("failed to wait for system rsync daemon: {error}")
            )
            .with_role(Role::Daemon);
            if write_message(&message, stderr).is_err() {
                let _ = writeln!(
                    stderr.writer_mut(),
                    "failed to wait for system rsync daemon: {error}"
                );
            }
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_core::fallback::{CLIENT_FALLBACK_ENV, DAEMON_FALLBACK_ENV};
    use std::env;
    use std::ffi::OsString;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    const TEST_FLAG: &str = "OC_RSYNC_DAEMON_TEST_FLAG";

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvSnapshot {
        entries: Vec<(&'static str, Option<OsString>)>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvSnapshot {
        fn new(keys: &'static [&'static str]) -> Self {
            let guard = env_lock()
                .lock()
                .expect("environment lock poisoned during test");
            let entries = keys
                .iter()
                .map(|&key| (key, env::var_os(key)))
                .collect::<Vec<_>>();
            Self {
                entries,
                _guard: guard,
            }
        }

        #[allow(unsafe_code)]
        fn set(&self, key: &'static str, value: &str) {
            debug_assert!(self.entries.iter().any(|(candidate, _)| *candidate == key));
            let owned = OsString::from(value);
            unsafe {
                env::set_var(key, &owned);
            }
        }

        #[allow(unsafe_code)]
        fn remove(&self, key: &'static str) {
            debug_assert!(self.entries.iter().any(|(candidate, _)| *candidate == key));
            unsafe {
                env::remove_var(key);
            }
        }
    }

    impl Drop for EnvSnapshot {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            for (key, value) in &self.entries {
                if let Some(original) = value {
                    unsafe {
                        env::set_var(key, original);
                    }
                } else {
                    unsafe {
                        env::remove_var(key);
                    }
                }
            }
        }
    }

    #[test]
    fn exit_code_from_clamps_values() {
        assert_eq!(exit_code_from(-1), std::process::ExitCode::from(0));
        assert_eq!(exit_code_from(42), std::process::ExitCode::from(42));
        assert_eq!(exit_code_from(9_999), std::process::ExitCode::from(u8::MAX));
    }

    #[test]
    fn env_flag_interprets_common_values() {
        let snapshot = EnvSnapshot::new(&[TEST_FLAG]);

        snapshot.remove(TEST_FLAG);
        assert_eq!(env_flag(TEST_FLAG), None);

        snapshot.set(TEST_FLAG, "false");
        assert_eq!(env_flag(TEST_FLAG), Some(false));

        snapshot.set(TEST_FLAG, "  ");
        assert_eq!(env_flag(TEST_FLAG), Some(true));

        snapshot.set(TEST_FLAG, "YES");
        assert_eq!(env_flag(TEST_FLAG), Some(true));

        snapshot.set(TEST_FLAG, "off");
        assert_eq!(env_flag(TEST_FLAG), Some(false));
    }

    #[test]
    fn auto_delegate_system_rsync_enabled_reads_environment() {
        let snapshot = EnvSnapshot::new(&[DAEMON_AUTO_DELEGATE_ENV]);

        snapshot.remove(DAEMON_AUTO_DELEGATE_ENV);
        assert!(!auto_delegate_system_rsync_enabled());

        snapshot.set(DAEMON_AUTO_DELEGATE_ENV, "1");
        assert!(auto_delegate_system_rsync_enabled());

        snapshot.set(DAEMON_AUTO_DELEGATE_ENV, "0");
        assert!(!auto_delegate_system_rsync_enabled());
    }

    #[test]
    fn fallback_binary_configured_accounts_for_disabling_overrides() {
        let snapshot = EnvSnapshot::new(&[DAEMON_FALLBACK_ENV, CLIENT_FALLBACK_ENV]);

        snapshot.remove(DAEMON_FALLBACK_ENV);
        snapshot.remove(CLIENT_FALLBACK_ENV);
        assert!(fallback_binary_configured());

        snapshot.set(DAEMON_FALLBACK_ENV, "0");
        assert!(!fallback_binary_configured());

        snapshot.remove(DAEMON_FALLBACK_ENV);
        snapshot.set(CLIENT_FALLBACK_ENV, "0");
        assert!(!fallback_binary_configured());
    }

    #[test]
    fn remainder_has_config_detects_flag_and_inline_form() {
        let args = [
            OsString::from("--config"),
            OsString::from("/tmp/file"),
            OsString::from("--daemon"),
        ];
        assert!(remainder_has_config(&args));

        let args2 = [
            OsString::from("--config=/tmp/file"),
            OsString::from("--daemon"),
        ];
        assert!(remainder_has_config(&args2));

        let args3 = [OsString::from("--daemon"), OsString::from("--no-detach")];
        assert!(!remainder_has_config(&args3));
    }
}
