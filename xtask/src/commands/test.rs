use crate::error::{TaskError, TaskResult};
#[cfg(test)]
use crate::util::is_help_flag;
use crate::util::run_cargo_tool;
use std::ffi::OsString;
use std::path::Path;

const NEXTEST_ARGS: &[&str] = &[
    "nextest",
    "run",
    "--workspace",
    "--all-targets",
    "--all-features",
];
const NEXTEST_DISPLAY: &str = "cargo nextest run --workspace --all-targets --all-features";
const NEXTEST_INSTALL_COMMAND: &str = "cargo install cargo-nextest --locked";
const NEXTEST_INSTALL_HINT: &str =
    "install cargo-nextest with `cargo install cargo-nextest --locked`";

const CARGO_TEST_ARGS: &[&str] = &["test", "--workspace", "--all-targets", "--all-features"];
const CARGO_TEST_DISPLAY: &str = "cargo test --workspace --all-targets --all-features";
const CARGO_TEST_INSTALL_HINT: &str = "install Rust and cargo from https://rustup.rs";

/// Options accepted by the `test` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TestOptions {
    /// Force running cargo test even when cargo-nextest is available.
    pub force_cargo_test: bool,
    /// Install cargo-nextest when missing before falling back to cargo test.
    pub install_nextest: bool,
}

/// Parses CLI arguments for the `test` command.
#[cfg(test)]
pub fn parse_args<I>(args: I) -> TaskResult<TestOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut options = TestOptions::default();

    for arg in args.into_iter() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        match arg.to_string_lossy().as_ref() {
            "--use-cargo-test" => options.force_cargo_test = true,
            "--install-nextest" => options.install_nextest = true,
            other => {
                return Err(TaskError::Usage(format!(
                    "unrecognised argument '{other}' for test command"
                )));
            }
        }
    }

    Ok(options)
}

/// Executes the `test` command.
pub fn execute(workspace: &Path, options: TestOptions) -> TaskResult<()> {
    if options.force_cargo_test {
        return run_cargo_tests(workspace);
    }

    match run_nextest(workspace) {
        Ok(()) => Ok(()),
        Err(TaskError::ToolMissing(message)) => {
            println!("{message}");
            if options.install_nextest {
                install_nextest(workspace)?;

                match run_nextest(workspace) {
                    Ok(()) => return Ok(()),
                    Err(TaskError::ToolMissing(message)) => println!("{message}"),
                    Err(other) => return Err(other),
                }
            }
            println!("{}", fallback_to_cargo_test_message());
            run_cargo_tests(workspace)
        }
        Err(other) => Err(other),
    }
}

fn fallback_to_cargo_test_message() -> String {
    format!(
        "Falling back to {CARGO_TEST_DISPLAY}; install cargo-nextest with `{NEXTEST_INSTALL_COMMAND}` \
         or rerun with --use-cargo-test to skip the probe"
    )
}

fn run_nextest(workspace: &Path) -> TaskResult<()> {
    run_cargo_tool(
        workspace,
        NEXTEST_ARGS.iter().map(OsString::from).collect(),
        NEXTEST_DISPLAY,
        NEXTEST_INSTALL_HINT,
    )
}

fn run_cargo_tests(workspace: &Path) -> TaskResult<()> {
    run_cargo_tool(
        workspace,
        CARGO_TEST_ARGS.iter().map(OsString::from).collect(),
        CARGO_TEST_DISPLAY,
        CARGO_TEST_INSTALL_HINT,
    )
}

fn install_nextest(workspace: &Path) -> TaskResult<()> {
    run_cargo_tool(
        workspace,
        NEXTEST_INSTALL_COMMAND
            .split_whitespace()
            .map(OsString::from)
            .collect(),
        NEXTEST_INSTALL_COMMAND,
        NEXTEST_INSTALL_HINT,
    )
}

/// Returns usage text for the command.
#[cfg(test)]
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask test [--use-cargo-test] [--install-nextest]\\n\\n\\\
Options:\\n  --use-cargo-test    Force running cargo test even when cargo-nextest is available\\n  --install-nextest   Install cargo-nextest when missing before falling back to cargo test\\n  -h, --help          Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TaskError;
    use crate::util::test_env::EnvGuard;
    use crate::workspace::workspace_root;

    const FORCE_MISSING_ENV: &str = "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS";

    #[test]
    fn parse_args_accepts_default_configuration() {
        let options = parse_args(std::iter::empty()).expect("parse succeeds");
        assert_eq!(options, TestOptions::default());
    }

    #[test]
    fn parse_args_supports_force_cargo_test_flag() {
        let options = parse_args([OsString::from("--use-cargo-test")]).expect("parse succeeds");
        assert!(options.force_cargo_test);
    }

    #[test]
    fn parse_args_supports_install_nextest_flag() {
        let options = parse_args([OsString::from("--install-nextest")]).expect("parse succeeds");
        assert!(options.install_nextest);
    }

    #[test]
    fn parse_args_reports_help_request() {
        let error = parse_args([OsString::from("--help")]).unwrap_err();
        assert!(matches!(error, TaskError::Help(message) if message == usage()));
    }

    #[test]
    fn parse_args_rejects_unknown_argument() {
        let error = parse_args([OsString::from("--unknown")]).unwrap_err();
        assert!(matches!(error, TaskError::Usage(message) if message.contains("test")));
    }

    #[test]
    fn execute_prefers_nextest_when_available() {
        let mut env = EnvGuard::new();
        env.set("CARGO", "true");
        let workspace = workspace_root().expect("workspace root");
        execute(workspace.as_path(), TestOptions::default()).expect("nextest invocation succeeds");
    }

    #[test]
    fn execute_falls_back_when_nextest_missing() {
        let mut env = EnvGuard::new();
        env.set("CARGO", "true");
        env.set(FORCE_MISSING_ENV, NEXTEST_DISPLAY);
        let workspace = workspace_root().expect("workspace root");
        execute(workspace.as_path(), TestOptions::default()).expect("fallback succeeds");
    }

    #[test]
    fn fallback_message_is_well_formed() {
        assert_eq!(
            fallback_to_cargo_test_message(),
            format!(
                "Falling back to {CARGO_TEST_DISPLAY}; install cargo-nextest with `{NEXTEST_INSTALL_COMMAND}` or rerun with --use-cargo-test to skip the probe"
            )
        );
    }

    #[test]
    fn execute_honours_force_cargo_test_flag() {
        let mut env = EnvGuard::new();
        env.set("CARGO", "true");
        env.set(FORCE_MISSING_ENV, NEXTEST_DISPLAY);
        let workspace = workspace_root().expect("workspace root");
        execute(
            workspace.as_path(),
            TestOptions {
                force_cargo_test: true,
                install_nextest: false,
            },
        )
        .expect("cargo test invocation succeeds");
    }

    #[test]
    fn execute_attempts_install_when_requested() {
        let mut env = EnvGuard::new();
        env.set("CARGO", "true");
        env.set(FORCE_MISSING_ENV, NEXTEST_DISPLAY);
        let workspace = workspace_root().expect("workspace root");

        execute(
            workspace.as_path(),
            TestOptions {
                force_cargo_test: false,
                install_nextest: true,
            },
        )
        .expect("install path falls back to cargo test");
    }
}
