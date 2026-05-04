use crate::support;
use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// Runs the shared client entry point for the `oc-rsync` binary.
///
/// The helper still honours legacy invocation names (for example, `rsync`)
/// so downstream packaging can provide compatibility symlinks without shipping
/// extra binaries. Centralising the logic keeps tests, packaging, and telemetry
/// focused on a single execution path. The helper forwards its arguments and
/// I/O handles to the CLI crate and normalises the returned status via the
/// shared exit-code mapper.
#[must_use]
pub fn run_with<I, Out, Err>(args: I, stdout: &mut Out, stderr: &mut Err) -> ExitCode
where
    I: IntoIterator,
    I::Item: Into<OsString>,
    Out: Write,
    Err: Write,
{
    support::dispatch(args, stdout, stderr, cli::run, cli::exit_code_from)
}

#[cfg(test)]
mod tests {
    use super::run_with;
    use core::version::{LEGACY_PROGRAM_NAME, PROGRAM_NAME, RUST_VERSION};
    use std::ffi::OsString;
    use std::process::ExitCode;

    fn client_program_names() -> Vec<&'static str> {
        if LEGACY_PROGRAM_NAME == PROGRAM_NAME {
            vec![PROGRAM_NAME]
        } else {
            vec![PROGRAM_NAME, LEGACY_PROGRAM_NAME]
        }
    }

    #[test]
    fn version_flag_reports_success_for_all_binaries() {
        for name in client_program_names() {
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
        for name in client_program_names() {
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

        let mut expected_stdout = Vec::new();
        let mut expected_stderr = Vec::new();
        let help_exit = cli::run(
            [PROGRAM_NAME, "--help"],
            &mut expected_stdout,
            &mut expected_stderr,
        );
        assert_eq!(
            help_exit, 0,
            "--help should succeed for the canonical program name"
        );
        assert!(
            expected_stderr.is_empty(),
            "--help must not write diagnostics to stderr"
        );
        assert_eq!(
            stdout, expected_stdout,
            "empty invocation should mirror --help output"
        );

        let stdout_text = String::from_utf8(stdout.clone()).expect("stdout is UTF-8");
        assert!(
            stdout_text.starts_with(&format!("{PROGRAM_NAME} ")),
            "help output should begin with the program banner"
        );
        assert!(
            stdout_text.contains(&format!("Usage: {PROGRAM_NAME} [-h]")),
            "help output should include the usage synopsis"
        );

        let stderr_text = String::from_utf8(stderr).expect("stderr is UTF-8");
        assert!(
            stderr_text.contains(&format!(
                "{PROGRAM_NAME} error: syntax or usage error (code 1)"
            )),
            "diagnostic should use the branded syntax-or-usage error wording"
        );
        assert!(
            stderr_text.contains(&format!("[client={RUST_VERSION}]")),
            "diagnostic should include the branded role trailer"
        );
    }

    #[test]
    fn legacy_brand_reports_usage_with_legacy_prefix() {
        if LEGACY_PROGRAM_NAME == PROGRAM_NAME {
            return;
        }

        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run_with(
            [OsString::from(LEGACY_PROGRAM_NAME)],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit, ExitCode::FAILURE);

        let stderr_text = String::from_utf8(stderr).expect("stderr is UTF-8");
        assert!(
            stderr_text.contains("rsync error: syntax or usage error (code 1)"),
            "legacy branded binary should render diagnostics using the upstream prefix"
        );
        assert!(
            stderr_text.contains(&format!("[client={RUST_VERSION}]")),
            "diagnostic should include the branded role trailer"
        );

        let mut expected_stdout = Vec::new();
        let mut expected_stderr = Vec::new();
        let help_exit = cli::run(
            [LEGACY_PROGRAM_NAME, "--help"],
            &mut expected_stdout,
            &mut expected_stderr,
        );
        assert_eq!(help_exit, 0, "--help should succeed for the legacy brand");
        assert!(
            expected_stderr.is_empty(),
            "--help must not report diagnostics for legacy brand"
        );
        assert_eq!(
            stdout, expected_stdout,
            "legacy binary should mirror --help output"
        );

        let stdout_text = String::from_utf8(stdout.clone()).expect("stdout is UTF-8");
        assert!(
            stdout_text.starts_with(&format!("{LEGACY_PROGRAM_NAME} ")),
            "legacy binary should render usage banner with upstream prefix"
        );
        assert!(
            stdout_text.contains(&format!("Usage: {LEGACY_PROGRAM_NAME} [-h]")),
            "legacy help output should include the usage synopsis"
        );
    }
}
