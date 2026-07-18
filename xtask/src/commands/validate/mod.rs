//! `cargo xtask validate` - drop-in fidelity matrix.
//!
//! Runs oc-rsync and upstream rsync as the pulling client over every client
//! transport (local, ssh subprocess, embedded russh, daemon) and asserts the
//! results are identical across content, metadata, ACLs, xattrs, and verbose /
//! progress output. Optionally benchmarks a 10k-file workload across the same
//! transports to feed performance work.
//!
//! Each check is a self-contained [`Check`] strategy that owns its fixture and
//! comparison and reports one [`CheckOutcome`] per matrix cell; the runner just
//! aggregates. This keeps checks independent and individually testable.

use std::path::Path;

use crate::error::TaskResult;

/// Platform-agnostic options parsed from the CLI.
///
/// Transports are carried as labels so this type compiles on every platform;
/// the Unix-only runner resolves them to concrete transports.
#[derive(Debug, Default)]
pub struct ValidateOptions {
    /// Transport labels to exercise; empty means all transports.
    pub transports: Vec<String>,
    /// Run the 10k-file performance benchmark after the correctness matrix.
    pub bench: bool,
    /// File count for the benchmark workload.
    pub bench_files: usize,
    /// Enrich fixtures with edge cases (opt-in).
    pub edge_cases: bool,
    /// Run root-only validations (device nodes, id remaps) when actually root.
    pub root: bool,
    /// Print each transfer's command and stdout on failure.
    pub verbose: bool,
}

impl From<crate::cli::ValidateMatrixArgs> for ValidateOptions {
    fn from(args: crate::cli::ValidateMatrixArgs) -> Self {
        ValidateOptions {
            transports: args.transports,
            bench: args.bench,
            bench_files: args.bench_files.unwrap_or(10_000),
            edge_cases: args.edge_cases,
            root: args.root,
            verbose: args.verbose,
        }
    }
}

#[cfg(unix)]
mod bench;
#[cfg(unix)]
mod checks;
#[cfg(unix)]
mod support;
#[cfg(unix)]
mod transport;

#[cfg(unix)]
pub use unix_impl::{Check, CheckOutcome, ValidateCtx, execute};

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use transport::Transport;

    /// One check's result for one matrix cell.
    pub struct CheckOutcome {
        /// Check name (e.g. `metadata`).
        pub check: &'static str,
        /// Cell label (usually a transport, e.g. `ssh-subprocess`).
        pub cell: String,
        /// Outcome of the cell.
        pub status: Status,
        /// One-line detail (diagnostic on failure, note otherwise).
        pub detail: String,
    }

    /// Pass / fail / skip for one cell.
    #[derive(PartialEq, Eq)]
    pub enum Status {
        /// oc matched upstream.
        Pass,
        /// oc diverged from upstream.
        Fail,
        /// Cell could not run (missing tool, unreachable sshd).
        Skip,
    }

    impl CheckOutcome {
        /// Construct a passing outcome.
        pub fn pass(check: &'static str, cell: impl Into<String>) -> Self {
            Self {
                check,
                cell: cell.into(),
                status: Status::Pass,
                detail: String::new(),
            }
        }
        /// Construct a failing outcome with a diagnostic.
        pub fn fail(
            check: &'static str,
            cell: impl Into<String>,
            detail: impl Into<String>,
        ) -> Self {
            Self {
                check,
                cell: cell.into(),
                status: Status::Fail,
                detail: detail.into(),
            }
        }
        /// Construct a skipped outcome with a reason.
        pub fn skip(
            check: &'static str,
            cell: impl Into<String>,
            reason: impl Into<String>,
        ) -> Self {
            Self {
                check,
                cell: cell.into(),
                status: Status::Skip,
                detail: reason.into(),
            }
        }
    }

    /// Shared context passed to every check.
    pub struct ValidateCtx<'a> {
        /// oc-rsync binary under test.
        pub oc: &'a Path,
        /// Upstream rsync binary (ground truth + remote sender).
        pub upstream: &'a Path,
        /// Scratch root; checks create subdirectories under it.
        pub work: &'a Path,
        /// Transports to exercise.
        pub transports: &'a [Transport],
        /// Enrich fixtures with edge cases (opt-in).
        pub edge_cases: bool,
        /// Run root-only validations when the process is actually root.
        pub root: bool,
        /// Verbose diagnostics.
        pub verbose: bool,
    }

    /// A single fidelity check. Owns its fixture and comparison; runs across the
    /// transports (or directions) it cares about, honoring `ctx.transports`.
    pub trait Check {
        /// Stable check name for the report.
        fn name(&self) -> &'static str;
        /// Run the check, returning one outcome per matrix cell.
        fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome>;
    }

    /// Resolve requested transport labels to concrete transports (all if empty).
    fn resolve_transports(labels: &[String]) -> TaskResult<Vec<Transport>> {
        if labels.is_empty() {
            return Ok(Transport::ALL.to_vec());
        }
        labels
            .iter()
            .map(|l| {
                Transport::parse(l).ok_or_else(|| {
                    crate::error::TaskError::Validation(format!("unknown transport `{l}`"))
                })
            })
            .collect()
    }

    /// Detect binaries, run every check, print the matrix, and fail if any cell
    /// diverged.
    pub fn execute(workspace: &Path, options: ValidateOptions) -> TaskResult<()> {
        let oc = crate::commands::interop::shared::oc_rsync::detect_oc_rsync_binary(workspace)?;
        let upstream = pick_upstream(workspace)?;
        let transports = resolve_transports(&options.transports)?;

        let work = workspace.join("target/validate");
        if work.exists() {
            let _ = std::fs::remove_dir_all(&work);
        }
        std::fs::create_dir_all(&work)
            .map_err(|e| crate::error::TaskError::Validation(format!("create work dir: {e}")))?;

        eprintln!(
            "[validate] oc-rsync={} vs upstream={} over [{}]",
            oc.binary_path().display(),
            upstream.display(),
            transports
                .iter()
                .map(|t| t.label())
                .collect::<Vec<_>>()
                .join(", ")
        );

        let ctx = ValidateCtx {
            oc: oc.binary_path(),
            upstream: &upstream,
            work: &work,
            transports: &transports,
            edge_cases: options.edge_cases,
            root: options.root,
            verbose: options.verbose,
        };

        let mut outcomes = Vec::new();
        for check in checks::all() {
            outcomes.extend(check.run(&ctx));
        }
        report(&outcomes);

        if options.bench {
            bench::run(&ctx, options.bench_files)?;
        }

        let failed = outcomes.iter().filter(|o| o.status == Status::Fail).count();
        if failed > 0 {
            return Err(crate::error::TaskError::Validation(format!(
                "{failed} fidelity check(s) diverged from upstream"
            )));
        }
        Ok(())
    }

    /// Ground-truth upstream rsync: prefer the system `rsync` on `PATH` (the
    /// real drop-in comparison target), else an interop-built binary (3.4.x
    /// preferred).
    fn pick_upstream(workspace: &Path) -> TaskResult<std::path::PathBuf> {
        if let Some(system) = system_rsync() {
            return Ok(system);
        }
        let all = crate::commands::interop::shared::upstream::detect_upstream_binaries(workspace)?;
        let available: Vec<_> = all.into_iter().filter(|b| b.is_available()).collect();
        let chosen = available
            .iter()
            .find(|b| b.version_string().starts_with("3.4"))
            .or_else(|| available.first())
            .ok_or_else(|| {
                crate::error::TaskError::Validation("no upstream rsync binary found".into())
            })?;
        Ok(chosen.binary_path().to_path_buf())
    }

    /// Locate a working `rsync` on `PATH`.
    fn system_rsync() -> Option<std::path::PathBuf> {
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("rsync");
            if candidate.is_file()
                && std::process::Command::new(&candidate)
                    .arg("--version")
                    .output()
                    .map(|out| out.status.success())
                    .unwrap_or(false)
            {
                return Some(candidate);
            }
        }
        None
    }

    /// Print the matrix grouped by check, plus a summary line.
    fn report(outcomes: &[CheckOutcome]) {
        let (mut pass, mut fail, mut skip) = (0, 0, 0);
        eprintln!("\n=== fidelity matrix ===");
        let mut current = "";
        for o in outcomes {
            if o.check != current {
                eprintln!("\n{}:", o.check);
                current = o.check;
            }
            let (mark, count) = match o.status {
                Status::Pass => ("PASS", &mut pass),
                Status::Fail => ("FAIL", &mut fail),
                Status::Skip => ("SKIP", &mut skip),
            };
            *count += 1;
            if o.detail.is_empty() {
                eprintln!("  [{mark}] {}", o.cell);
            } else {
                eprintln!("  [{mark}] {} - {}", o.cell, o.detail);
            }
        }
        eprintln!("\n=== {pass} passed, {fail} failed, {skip} skipped ===");
    }
}

#[cfg(not(unix))]
pub fn execute(_workspace: &Path, _options: ValidateOptions) -> TaskResult<()> {
    Err(crate::error::TaskError::Validation(
        "`cargo xtask validate` is only supported on Unix hosts".into(),
    ))
}
