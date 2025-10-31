#![deny(unsafe_code)]

mod support;

use std::io::Write;
use std::{env, io, process::ExitCode};

fn main() -> ExitCode {
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    rsync_windows_gnu_eh::force_link();

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    run_with(env::args_os(), &mut stdout, &mut stderr)
}

fn run_with<I, Out, Err>(args: I, stdout: &mut Out, stderr: &mut Err) -> ExitCode
where
    I: IntoIterator,
    I::Item: Into<std::ffi::OsString>,
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
    use std::ffi::OsString;
    use std::process::ExitCode;

    #[test]
    fn version_flag_reports_success() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run_with(["oc-rsync", "--version"], &mut stdout, &mut stderr);
        assert_eq!(exit, ExitCode::SUCCESS);
        assert!(!stdout.is_empty(), "version output should not be empty");
        assert!(stderr.is_empty(), "version flag should not write to stderr");
    }

    #[test]
    fn unknown_flag_reports_failure() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = run_with(
            [
                OsString::from("oc-rsync"),
                OsString::from("--definitely-invalid-option"),
            ],
            &mut stdout,
            &mut stderr,
        );

        assert_eq!(exit, ExitCode::FAILURE);
        assert!(
            stdout.is_empty(),
            "unexpected stdout for invalid flag: {:?}",
            stdout
        );
        assert!(
            !stderr.is_empty(),
            "invalid flag should emit diagnostics to stderr"
        );
    }
}
