//! Single-command all-platform cross-check for pre-push verification.
//!
//! Runs the exact required-CI cross-platform gates locally, one cargo
//! invocation at a time (they share the workspace build lock), and prints a
//! per-target pass/fail summary so cross-platform breaks are caught before CI:
//!
//! 1. `cargo clippy --locked --workspace --all-targets --all-features --no-deps -- -D warnings`
//! 2. `cargo check  --locked --workspace --target x86_64-pc-windows-gnu`
//! 3. `cargo check  --locked --workspace --target x86_64-unknown-linux-musl`
//!
//! When a rustup target or the matching cross toolchain (linker / C compiler)
//! is absent, the target is SKIPPED with a clearly labelled, actionable hint
//! (for example `rustup target add ...`, install mingw-w64 or musl) rather than
//! failing with a raw linker error - and it is never silently reported as
//! passing.
//!
//! # Why windows-MSVC is intentionally excluded
//!
//! `x86_64-pc-windows-msvc` cannot be cross-compiled from a non-Windows host:
//! it needs the MSVC toolchain (`cl.exe` / `lib.exe`), which is unavailable off
//! Windows. The CI Windows runner is the authoritative check for that target,
//! and `x86_64-pc-windows-gnu` exercises the same `cfg(windows)` code paths, so
//! it is the portable stand-in used here.

use crate::error::{TaskError, TaskResult};
use crate::util::{ensure_command_available, run_cargo_tool};
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

/// A cross toolchain component that must be present on `PATH` before a target
/// can be checked (its cross linker / C compiler).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RequiredTool {
    /// Executable that must be discoverable on `PATH`.
    program: &'static str,
    /// Actionable hint printed when the tool is absent.
    hint: &'static str,
}

/// Descriptor for one cross-check target (Strategy/table entry).
///
/// Each entry pairs a human-readable label with the cargo arguments to run and
/// the optional rustup target triple and cross toolchain the check depends on.
#[derive(Clone, Copy, Debug)]
struct CrossTarget {
    /// Human-readable label shown in the summary.
    label: &'static str,
    /// Rustup target triple, or `None` for the host clippy lint pass.
    triple: Option<&'static str>,
    /// Cargo arguments (everything after the `cargo` program name).
    cargo_args: &'static [&'static str],
    /// Display string for diagnostics and skip messages.
    display: &'static str,
    /// Cross linker / C compiler required on `PATH`, if any.
    linker: Option<RequiredTool>,
}

/// Outcome of evaluating a single [`CrossTarget`].
#[derive(Clone, Debug, Eq, PartialEq)]
enum CheckOutcome {
    /// The cargo invocation succeeded.
    Passed,
    /// The cargo invocation ran and failed.
    Failed,
    /// The target was skipped because a prerequisite was absent; carries the
    /// actionable hint to surface to the operator.
    Skipped(String),
}

const CLIPPY_ARGS: &[&str] = &[
    "clippy",
    "--locked",
    "--workspace",
    "--all-targets",
    "--all-features",
    "--no-deps",
    "--",
    "-D",
    "warnings",
];

const WINDOWS_GNU_ARGS: &[&str] = &[
    "check",
    "--locked",
    "--workspace",
    "--target",
    "x86_64-pc-windows-gnu",
];

const MUSL_ARGS: &[&str] = &[
    "check",
    "--locked",
    "--workspace",
    "--target",
    "x86_64-unknown-linux-musl",
];

/// Hint used when cargo itself cannot be spawned.
const CARGO_HINT: &str = "install the Rust toolchain from https://rustup.rs";

/// Returns the ordered cross-check target table.
///
/// windows-MSVC is intentionally absent: it cannot be cross-compiled from a
/// non-Windows host (no `cl.exe` / `lib.exe`); the CI msvc runner is
/// authoritative and windows-gnu covers the same `cfg(windows)` code.
fn targets() -> &'static [CrossTarget] {
    &[
        CrossTarget {
            label: "host clippy (-D warnings)",
            triple: None,
            cargo_args: CLIPPY_ARGS,
            display: "cargo clippy --locked --workspace --all-targets --all-features --no-deps -- -D warnings",
            linker: None,
        },
        CrossTarget {
            label: "windows (x86_64-pc-windows-gnu)",
            triple: Some("x86_64-pc-windows-gnu"),
            cargo_args: WINDOWS_GNU_ARGS,
            display: "cargo check --locked --workspace --target x86_64-pc-windows-gnu",
            linker: Some(RequiredTool {
                program: "x86_64-w64-mingw32-gcc",
                hint: "install mingw-w64 (e.g. `pacman -S mingw-w64-gcc`, `apt install gcc-mingw-w64`, or `brew install mingw-w64`)",
            }),
        },
        CrossTarget {
            label: "linux musl (x86_64-unknown-linux-musl)",
            triple: Some("x86_64-unknown-linux-musl"),
            cargo_args: MUSL_ARGS,
            display: "cargo check --locked --workspace --target x86_64-unknown-linux-musl",
            linker: Some(RequiredTool {
                program: "musl-gcc",
                hint: "install the musl toolchain (e.g. `pacman -S musl`, `apt install musl-tools`, or `brew install FiloSottile/musl-cross/musl-cross`)",
            }),
        },
    ]
}

/// Environment abstraction over the operations a cross-check performs.
///
/// Injecting this behind a trait lets tests exercise the skip / hint / summary
/// logic with a mock, so the unit tests never shell out to cargo or rustup.
trait CheckEnv {
    /// Returns whether a rustup target triple is installed.
    fn target_installed(&self, triple: &str) -> TaskResult<bool>;
    /// Returns whether an executable is discoverable on `PATH`.
    fn command_available(&self, program: &str) -> bool;
    /// Runs a cargo invocation, mapping failures to [`TaskError`].
    fn run(&self, args: &[&str], display: &str) -> TaskResult<()>;
}

/// Live [`CheckEnv`] backed by the real system toolchain.
struct SystemEnv<'a> {
    /// Workspace root the cargo invocations run in.
    workspace: &'a Path,
}

impl CheckEnv for SystemEnv<'_> {
    fn target_installed(&self, triple: &str) -> TaskResult<bool> {
        ensure_command_available(
            "rustup",
            "install rustup from https://rustup.rs to manage Rust toolchains",
        )?;

        let output = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()?;

        if !output.status.success() {
            return Err(TaskError::CommandFailed {
                program: "rustup target list --installed".to_owned(),
                status: output.status,
            });
        }

        let installed = String::from_utf8_lossy(&output.stdout);
        Ok(installed.lines().any(|line| line.trim() == triple))
    }

    fn command_available(&self, program: &str) -> bool {
        ensure_command_available(program, "").is_ok()
    }

    fn run(&self, args: &[&str], display: &str) -> TaskResult<()> {
        let args_os: Vec<OsString> = args.iter().map(OsString::from).collect();
        run_cargo_tool(self.workspace, args_os, display, CARGO_HINT)
    }
}

/// Evaluates a single target, short-circuiting to a labelled skip when a
/// prerequisite (rustup target or cross toolchain) is absent.
fn check_target(env: &dyn CheckEnv, target: &CrossTarget) -> CheckOutcome {
    if let Some(triple) = target.triple {
        match env.target_installed(triple) {
            Ok(true) => {}
            Ok(false) => {
                return CheckOutcome::Skipped(format!(
                    "rustup target '{triple}' is not installed; add it with `rustup target add {triple}`"
                ));
            }
            Err(error) => {
                return CheckOutcome::Skipped(format!(
                    "could not determine whether rustup target '{triple}' is installed: {error}"
                ));
            }
        }
    }

    if let Some(tool) = target.linker {
        if !env.command_available(tool.program) {
            return CheckOutcome::Skipped(format!(
                "cross toolchain '{}' was not found on PATH; {}",
                tool.program, tool.hint
            ));
        }
    }

    match env.run(target.cargo_args, target.display) {
        Ok(()) => CheckOutcome::Passed,
        // A missing cargo subcommand (e.g. clippy component absent) is a skip,
        // not a failure of the code under check.
        Err(TaskError::ToolMissing(message)) => CheckOutcome::Skipped(message),
        Err(_) => CheckOutcome::Failed,
    }
}

/// Evaluates every target in order and returns their outcomes paired with the
/// descriptor. Pure over the injected [`CheckEnv`] so it is unit-testable.
fn evaluate<'a>(
    env: &dyn CheckEnv,
    targets: &'a [CrossTarget],
) -> Vec<(&'a CrossTarget, CheckOutcome)> {
    targets
        .iter()
        .map(|target| (target, check_target(env, target)))
        .collect()
}

/// Renders the per-target summary and returns a [`TaskError`] when any target
/// failed. Skips are surfaced loudly (never silently treated as a pass).
fn report(outcomes: &[(&CrossTarget, CheckOutcome)]) -> TaskResult<()> {
    println!("cross-check summary:");

    let mut failed = 0usize;
    let mut skipped = 0usize;

    for (target, outcome) in outcomes {
        match outcome {
            CheckOutcome::Passed => println!("  PASS  {}", target.label),
            CheckOutcome::Failed => {
                failed += 1;
                println!("  FAIL  {}", target.label);
            }
            CheckOutcome::Skipped(reason) => {
                skipped += 1;
                println!("  SKIP  {} - {reason}", target.label);
            }
        }
    }

    if skipped > 0 {
        println!(
            "warning: {skipped} target(s) skipped for a missing toolchain and were NOT verified locally; CI remains authoritative for them"
        );
    }

    if failed > 0 {
        return Err(TaskError::Validation(format!(
            "cross-check: {failed} target(s) failed"
        )));
    }

    println!("cross-check: all runnable targets passed");
    Ok(())
}

/// Executes the `cross-check` command.
pub fn execute(workspace: &Path) -> TaskResult<()> {
    let env = SystemEnv { workspace };
    let outcomes = evaluate(&env, targets());
    report(&outcomes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    /// Scripted run result for the mock environment.
    #[derive(Clone, Copy)]
    enum MockRun {
        Ok,
        Failed,
        Missing,
    }

    /// Mock [`CheckEnv`] that records invocations and returns scripted results,
    /// so tests never spawn cargo or rustup.
    struct MockEnv {
        installed: HashSet<&'static str>,
        available: HashSet<&'static str>,
        run_results: HashMap<&'static str, MockRun>,
        runs: RefCell<Vec<String>>,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                installed: HashSet::new(),
                available: HashSet::new(),
                run_results: HashMap::new(),
                runs: RefCell::new(Vec::new()),
            }
        }

        fn with_target(mut self, triple: &'static str) -> Self {
            self.installed.insert(triple);
            self
        }

        fn with_tool(mut self, program: &'static str) -> Self {
            self.available.insert(program);
            self
        }

        fn with_run(mut self, display: &'static str, result: MockRun) -> Self {
            self.run_results.insert(display, result);
            self
        }
    }

    impl CheckEnv for MockEnv {
        fn target_installed(&self, triple: &str) -> TaskResult<bool> {
            Ok(self.installed.contains(triple))
        }

        fn command_available(&self, program: &str) -> bool {
            self.available.contains(program)
        }

        fn run(&self, _args: &[&str], display: &str) -> TaskResult<()> {
            self.runs.borrow_mut().push(display.to_owned());
            match self.run_results.get(display).copied() {
                Some(MockRun::Ok) | None => Ok(()),
                Some(MockRun::Failed) => Err(TaskError::CommandFailed {
                    program: display.to_owned(),
                    status: exit_status(1),
                }),
                Some(MockRun::Missing) => {
                    Err(TaskError::ToolMissing(format!("{display} is unavailable")))
                }
            }
        }
    }

    /// Builds an `ExitStatus` with the given code without spawning a process.
    fn exit_status(code: i32) -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            std::process::ExitStatus::from_raw((code & 0xff) << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt;
            std::process::ExitStatus::from_raw(code as u32)
        }
    }

    fn find(triple: Option<&str>) -> &'static CrossTarget {
        targets()
            .iter()
            .find(|t| t.triple == triple)
            .expect("target present in table")
    }

    #[test]
    fn table_lists_exactly_the_required_ci_targets() {
        let table = targets();
        assert_eq!(table.len(), 3, "clippy + windows-gnu + musl");
        assert!(
            table
                .iter()
                .all(|t| t.triple != Some("x86_64-pc-windows-msvc")),
            "windows-MSVC must not appear: it cannot be cross-compiled off Windows"
        );
        // The clippy pass is the sole host (no triple) entry and gates warnings.
        let clippy = find(None);
        assert!(clippy.cargo_args.contains(&"clippy"));
        assert!(clippy.cargo_args.contains(&"-D"));
        assert!(clippy.cargo_args.contains(&"warnings"));
        assert!(clippy.cargo_args.contains(&"--locked"));
        // Both cross targets pin --locked and their triple.
        for triple in ["x86_64-pc-windows-gnu", "x86_64-unknown-linux-musl"] {
            let t = find(Some(triple));
            assert!(t.cargo_args.contains(&"check"));
            assert!(t.cargo_args.contains(&"--locked"));
            assert!(t.cargo_args.contains(&triple));
            assert!(
                t.linker.is_some(),
                "cross target must declare a linker probe"
            );
        }
    }

    #[test]
    fn passes_when_target_and_toolchain_present() {
        let target = find(Some("x86_64-unknown-linux-musl"));
        let env = MockEnv::new()
            .with_target("x86_64-unknown-linux-musl")
            .with_tool("musl-gcc")
            .with_run(target.display, MockRun::Ok);
        assert_eq!(check_target(&env, target), CheckOutcome::Passed);
        assert_eq!(env.runs.borrow().len(), 1, "cargo ran exactly once");
    }

    #[test]
    fn skips_with_hint_when_rustup_target_missing() {
        let target = find(Some("x86_64-pc-windows-gnu"));
        // Toolchain present, but the rustup target is not installed.
        let env = MockEnv::new().with_tool("x86_64-w64-mingw32-gcc");
        match check_target(&env, target) {
            CheckOutcome::Skipped(hint) => {
                assert!(
                    hint.contains("rustup target add x86_64-pc-windows-gnu"),
                    "{hint}"
                );
            }
            other => panic!("expected skip, got {other:?}"),
        }
        assert!(
            env.runs.borrow().is_empty(),
            "cargo must not run when target absent"
        );
    }

    #[test]
    fn skips_with_hint_when_cross_linker_missing() {
        let target = find(Some("x86_64-pc-windows-gnu"));
        // Rustup target installed, but the mingw-w64 linker is absent.
        let env = MockEnv::new().with_target("x86_64-pc-windows-gnu");
        match check_target(&env, target) {
            CheckOutcome::Skipped(hint) => {
                assert!(hint.contains("x86_64-w64-mingw32-gcc"), "{hint}");
                assert!(hint.contains("mingw-w64"), "{hint}");
            }
            other => panic!("expected skip, got {other:?}"),
        }
        assert!(
            env.runs.borrow().is_empty(),
            "cargo must not run when linker absent"
        );
    }

    #[test]
    fn musl_skip_hint_names_musl_toolchain() {
        let target = find(Some("x86_64-unknown-linux-musl"));
        let env = MockEnv::new().with_target("x86_64-unknown-linux-musl");
        match check_target(&env, target) {
            CheckOutcome::Skipped(hint) => {
                assert!(hint.contains("musl-gcc"), "{hint}");
                assert!(hint.contains("musl"), "{hint}");
            }
            other => panic!("expected skip, got {other:?}"),
        }
    }

    #[test]
    fn clippy_missing_component_is_skip_not_failure() {
        let clippy = find(None);
        let env = MockEnv::new().with_run(clippy.display, MockRun::Missing);
        assert!(
            matches!(check_target(&env, clippy), CheckOutcome::Skipped(_)),
            "a missing clippy component must be surfaced as a skip, not a fail"
        );
    }

    #[test]
    fn run_failure_is_reported_as_failed() {
        let clippy = find(None);
        let env = MockEnv::new().with_run(clippy.display, MockRun::Failed);
        assert_eq!(check_target(&env, clippy), CheckOutcome::Failed);
    }

    #[test]
    fn report_errors_only_when_a_target_failed() {
        let table = targets();
        let clippy = &table[0];
        let win = &table[1];

        let all_pass = vec![
            (clippy, CheckOutcome::Passed),
            (win, CheckOutcome::Skipped("no toolchain".to_owned())),
        ];
        report(&all_pass).expect("skips alone do not fail the command");

        let with_failure = vec![(clippy, CheckOutcome::Failed)];
        let error = report(&with_failure).unwrap_err();
        assert!(matches!(
            error,
            TaskError::Validation(message) if message.contains("1 target(s) failed")
        ));
    }

    #[test]
    fn evaluate_runs_every_runnable_target_once() {
        let table = targets();
        let env = MockEnv::new()
            .with_target("x86_64-pc-windows-gnu")
            .with_tool("x86_64-w64-mingw32-gcc")
            .with_target("x86_64-unknown-linux-musl")
            .with_tool("musl-gcc");
        let outcomes = evaluate(&env, table);
        assert_eq!(outcomes.len(), 3);
        assert!(outcomes.iter().all(|(_, o)| *o == CheckOutcome::Passed));
        assert_eq!(
            env.runs.borrow().len(),
            3,
            "one cargo invocation per target"
        );
    }
}
