use crate::support;
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// Runs the shared daemon entry point for every branded executable.
///
/// The canonical `rsyncd` binary and the `oc-rsyncd` compatibility
/// wrapper both forward to this helper so argument dispatch and status
/// mapping stay consistent across brands.
#[must_use]
pub fn run_with<I, Out, Err>(args: I, stdout: &mut Out, stderr: &mut Err) -> ExitCode
where
    I: IntoIterator,
    I::Item: Into<OsString>,
    Out: Write,
    Err: Write,
{
    support::dispatch(
        args,
        stdout,
        stderr,
        rsync_daemon::run,
        rsync_daemon::exit_code_from,
    )
}

#[cfg(test)]
mod tests {
    use super::run_with;
    use rsync_core::version::{DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME};
    use std::ffi::OsString;
    use std::process::ExitCode;

    const DAEMON_NAMES: &[&str] = &[DAEMON_PROGRAM_NAME, OC_DAEMON_PROGRAM_NAME];

    #[test]
    fn version_flag_reports_success_for_all_binaries() {
        for &name in DAEMON_NAMES {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let exit = run_with([name, "--version"], &mut stdout, &mut stderr);

            assert_eq!(exit, ExitCode::SUCCESS, "{name} --version should succeed");
            assert!(
                !stdout.is_empty(),
                "{name} --version should print to stdout"
            );
            assert!(
                stderr.is_empty(),
                "{name} --version must not write to stderr"
            );
        }
    }

    #[test]
    fn unknown_flag_reports_failure_for_all_binaries() {
        for &name in DAEMON_NAMES {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let exit = run_with(
                [
                    OsString::from(name),
                    OsString::from("--definitely-invalid-option"),
                ],
                &mut stdout,
                &mut stderr,
            );

            assert_eq!(
                exit,
                ExitCode::FAILURE,
                "{name} should reject invalid flags"
            );
            assert!(stdout.is_empty(), "invalid flag should not write to stdout");
            assert!(!stderr.is_empty(), "invalid flag should emit diagnostics");
        }
    }
}
