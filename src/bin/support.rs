#![deny(unsafe_code)]

use std::ffi::OsString;
use std::io::Write;
use std::process::ExitCode;

/// Runs a CLI entry-point by delegating to the provided runner and exit mapper.
///
/// The helper centralises the common pattern shared by the client and daemon
/// binaries: forward the captured argument iterator to the inner command,
/// stream diagnostics through caller-supplied writers, and normalise the
/// resulting status code into an [`ExitCode`].
#[allow(clippy::module_name_repetitions)]
pub fn dispatch<I, S, Out, Err, Runner, Mapper>(
    arguments: I,
    stdout: &mut Out,
    stderr: &mut Err,
    runner: Runner,
    exit_mapper: Mapper,
) -> ExitCode
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
    Out: Write,
    Err: Write,
    Runner: FnOnce(I, &mut Out, &mut Err) -> i32,
    Mapper: FnOnce(i32) -> ExitCode,
{
    let status = runner(arguments, stdout, stderr);
    exit_mapper(status)
}

#[cfg(test)]
mod tests {
    use super::dispatch;
    use std::ffi::OsString;
    use std::io::{self, Write};
    use std::process::ExitCode;

    #[test]
    fn dispatch_runs_runner_and_maps_exit_code() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exit = dispatch(
            ["program", "--flag"],
            &mut stdout,
            &mut stderr,
            |args, out: &mut Vec<u8>, err: &mut Vec<u8>| {
                let collected: Vec<OsString> = args.into_iter().map(Into::into).collect();
                assert_eq!(
                    collected,
                    vec![OsString::from("program"), OsString::from("--flag")]
                );
                writeln!(out, "stdout").unwrap();
                writeln!(err, "stderr").unwrap();
                42
            },
            |status| ExitCode::from(status as u8),
        );

        assert_eq!(exit, ExitCode::from(42));
        assert_eq!(stdout, b"stdout\n");
        assert_eq!(stderr, b"stderr\n");
    }

    #[test]
    fn dispatch_accepts_custom_exit_mapper() {
        let mut stdout = io::sink();
        let mut stderr = io::sink();
        let exit = dispatch(
            ["binary"],
            &mut stdout,
            &mut stderr,
            |_args, _out: &mut io::Sink, _err: &mut io::Sink| 300,
            |status| ExitCode::from(status.saturating_sub(200) as u8),
        );

        assert_eq!(exit, ExitCode::from(100));
    }
}
