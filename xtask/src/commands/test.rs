use crate::error::{TaskError, TaskResult};
use crate::util::{is_help_flag, run_cargo_tool};
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
const NEXTEST_INSTALL_HINT: &str =
    "install cargo-nextest with `cargo install cargo-nextest --locked`";

const CARGO_TEST_ARGS: &[&str] = &["test", "--workspace", "--all-targets", "--all-features"];
const CARGO_TEST_DISPLAY: &str = "cargo test --workspace --all-targets --all-features";
const CARGO_TEST_INSTALL_HINT: &str = "install Rust and cargo from https://rustup.rs";

/// Options accepted by the `test` command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TestOptions {
    force_cargo_test: bool,
}

/// Parses CLI arguments for the `test` command.
pub fn parse_args<I>(args: I) -> TaskResult<TestOptions>
where
    I: IntoIterator<Item = OsString>,
{
    let mut args = args.into_iter();
    let mut options = TestOptions::default();

    while let Some(arg) = args.next() {
        if is_help_flag(&arg) {
            return Err(TaskError::Help(usage()));
        }

        match arg.to_string_lossy().as_ref() {
            "--use-cargo-test" => options.force_cargo_test = true,
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
            println!("{message}; falling back to cargo test");
            run_cargo_tests(workspace)
        }
        Err(other) => Err(other),
    }
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

/// Returns usage text for the command.
pub fn usage() -> String {
    String::from(
        "Usage: cargo xtask test [--use-cargo-test]\\n\\n\\\
Options:\\n  --use-cargo-test  Force running cargo test even when cargo-nextest is available\\n  -h, --help        Show this help message",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TaskError;
    use crate::workspace::workspace_root;
    use std::env;

    const FORCE_MISSING_ENV: &str = "OC_RSYNC_FORCE_MISSING_CARGO_TOOLS";

    struct EnvGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvGuard {
        #[allow(unsafe_code)]
        fn set(key: &'static str, value: &str) -> Self {
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

    fn guard_cargo(value: &str) -> EnvGuard {
        EnvGuard::set("CARGO", value)
    }

    fn guard_force_missing(value: &str) -> EnvGuard {
        EnvGuard::set(FORCE_MISSING_ENV, value)
    }

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
        let _cargo = guard_cargo("true");
        let workspace = workspace_root().expect("workspace root");
        execute(workspace.as_path(), TestOptions::default()).expect("nextest invocation succeeds");
    }

    #[test]
    fn execute_falls_back_when_nextest_missing() {
        let _cargo = guard_cargo("true");
        let _missing = guard_force_missing(NEXTEST_DISPLAY);
        let workspace = workspace_root().expect("workspace root");
        execute(workspace.as_path(), TestOptions::default()).expect("fallback succeeds");
    }

    #[test]
    fn execute_honours_force_cargo_test_flag() {
        let _cargo = guard_cargo("true");
        let _missing = guard_force_missing(NEXTEST_DISPLAY);
        let workspace = workspace_root().expect("workspace root");
        execute(
            workspace.as_path(),
            TestOptions {
                force_cargo_test: true,
            },
        )
        .expect("cargo test invocation succeeds");
    }
}
