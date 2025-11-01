use crate::support;
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// Runs the shared client entry point for every branded executable.
///
/// Both branded binaries—the upstream-compatible client exposed as
/// `rsync_core::version::PROGRAM_NAME` and the oc-branded wrapper
/// published as `rsync_core::version::OC_PROGRAM_NAME`—call into this
/// helper. Centralising the logic keeps tests, packaging, and telemetry
/// focused on a single execution path. The helper forwards its arguments
/// and I/O handles to the CLI crate and normalises the returned status via
/// the shared exit-code mapper.
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
        rsync_cli::run,
        rsync_cli::exit_code_from,
    )
}

#[cfg(test)]
mod tests {
    use super::run_with;
    use rsync_core::version::{OC_PROGRAM_NAME, PROGRAM_NAME};
    use std::ffi::OsString;
    use std::process::ExitCode;

    const CLIENT_NAMES: &[&str] = &[PROGRAM_NAME, OC_PROGRAM_NAME];

    #[test]
    fn version_flag_reports_success_for_all_binaries() {
        for &name in CLIENT_NAMES {
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
        for &name in CLIENT_NAMES {
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

    #[test]
    fn empty_argument_list_defaults_to_usage_error() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run_with(std::iter::empty::<OsString>(), &mut stdout, &mut stderr);

        assert_eq!(exit, ExitCode::FAILURE);

        let stdout_text = String::from_utf8(stdout).expect("stdout is UTF-8");
        assert!(stdout_text.contains("Usage:"));
        assert!(
            stdout_text.contains(PROGRAM_NAME),
            "usage banner should mention the canonical program name"
        );

        let stderr_text = String::from_utf8(stderr).expect("stderr is UTF-8");
        assert!(
            stderr_text.contains("missing source operands"),
            "diagnostic should explain that operands are required"
        );
    }
}
