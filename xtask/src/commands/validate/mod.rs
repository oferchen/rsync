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

use std::collections::HashSet;
use std::path::Path;

use crate::error::TaskResult;

/// A group a [`Check`] belongs to, used to select subsets of the matrix from
/// the CLI. A check may belong to several categories.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Category {
    /// Core drop-in fidelity checks (the default set).
    Validation,
    /// Checks that exercise enriched, adversarial fixtures.
    EdgeCases,
    /// Security-sensitive behaviours (path escapes, privilege handling).
    Security,
    /// Wire-format and protocol-level fidelity.
    Wire,
}

impl Category {
    /// Every category, in display order.
    pub const ALL: [Category; 4] = [
        Category::Validation,
        Category::EdgeCases,
        Category::Security,
        Category::Wire,
    ];

    /// Stable lowercase label matching the CLI flag that selects it.
    pub fn label(self) -> &'static str {
        match self {
            Category::Validation => "validation",
            Category::EdgeCases => "edge-cases",
            Category::Security => "security",
            Category::Wire => "wire",
        }
    }
}

/// Platform-agnostic options parsed from the CLI.
///
/// Transports are carried as labels so this type compiles on every platform;
/// the Unix-only runner resolves them to concrete transports.
#[derive(Debug, Default)]
pub struct ValidateOptions {
    /// Transport labels to exercise; empty means all transports.
    pub transports: Vec<String>,
    /// Ad-hoc rsync flag sets to validate for parity (each is one scenario).
    pub flags: Vec<String>,
    /// Run the 10k-file performance benchmark after the correctness matrix.
    pub bench: bool,
    /// File count for the benchmark workload.
    pub bench_files: usize,
    /// Select the [`Category::Validation`] checks.
    pub validation: bool,
    /// Enrich fixtures with edge cases and select [`Category::EdgeCases`].
    pub edge_cases: bool,
    /// Select the [`Category::Security`] checks.
    pub security: bool,
    /// Select the [`Category::Wire`] checks.
    pub wire: bool,
    /// List the available checks grouped by category, then exit.
    pub list: bool,
    /// Run root-only validations (device nodes, id remaps) when actually root.
    pub root: bool,
    /// Print each transfer's command and stdout on failure.
    pub verbose: bool,
}

impl From<crate::cli::ValidateMatrixArgs> for ValidateOptions {
    fn from(args: crate::cli::ValidateMatrixArgs) -> Self {
        ValidateOptions {
            transports: args.transports,
            flags: args.flags,
            bench: args.bench,
            bench_files: args.bench_files.unwrap_or(10_000),
            validation: args.validation,
            edge_cases: args.edge_cases,
            security: args.security,
            wire: args.wire,
            list: args.list,
            root: args.root,
            verbose: args.verbose,
        }
    }
}

/// Resolve the set of categories to run from the parsed flags.
///
/// When no category flag is passed the harness defaults to the historical
/// [`Category::Validation`] set, keeping a bare `cargo xtask validate`
/// regression-safe. `--edge-cases` both enriches fixtures and selects
/// [`Category::EdgeCases`].
pub fn selected_categories(options: &ValidateOptions) -> HashSet<Category> {
    let mut set = HashSet::new();
    if options.validation {
        set.insert(Category::Validation);
    }
    if options.edge_cases {
        set.insert(Category::EdgeCases);
    }
    if options.security {
        set.insert(Category::Security);
    }
    if options.wire {
        set.insert(Category::Wire);
    }
    if set.is_empty() {
        set.insert(Category::Validation);
    }
    set
}

#[cfg(unix)]
mod bench;
#[cfg(unix)]
mod checks;
#[cfg(unix)]
mod comparison;
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
        /// Ad-hoc rsync flag sets to validate for parity (each is one scenario).
        pub flags: &'a [String],
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
        /// Categories this check belongs to. Defaults to
        /// [`Category::Validation`] so existing checks need no changes.
        fn categories(&self) -> &'static [Category] {
            &[Category::Validation]
        }
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
        let all_checks = checks::all();
        if options.list {
            list_checks(&all_checks);
            return Ok(());
        }

        let selected = super::selected_categories(&options);
        let checks: Vec<Box<dyn Check>> = all_checks
            .into_iter()
            .filter(|c| c.categories().iter().any(|cat| selected.contains(cat)))
            .collect();

        let oc = crate::commands::interop::shared::oc_rsync::detect_oc_rsync_binary(workspace)?;
        let upstream = pick_upstream(workspace)?;
        let transports = resolve_transports(&options.transports)?;

        let work = workspace.join("target/validate");
        if work.exists() {
            let _ = std::fs::remove_dir_all(&work);
        }
        std::fs::create_dir_all(&work)
            .map_err(|e| crate::error::TaskError::Validation(format!("create work dir: {e}")))?;

        let mut cats: Vec<&'static str> = Category::ALL
            .iter()
            .filter(|c| selected.contains(c))
            .map(|c| c.label())
            .collect();
        cats.sort_unstable();
        eprintln!(
            "[validate] oc-rsync={} vs upstream={} over [{}] categories [{}]",
            oc.binary_path().display(),
            upstream.display(),
            transports
                .iter()
                .map(|t| t.label())
                .collect::<Vec<_>>()
                .join(", "),
            cats.join(", ")
        );

        let ctx = ValidateCtx {
            oc: oc.binary_path(),
            upstream: &upstream,
            work: &work,
            transports: &transports,
            flags: &options.flags,
            edge_cases: options.edge_cases,
            root: options.root,
            verbose: options.verbose,
        };

        let mut outcomes = Vec::new();
        for check in &checks {
            outcomes.extend(check.run(&ctx));
        }
        report(&outcomes);
        report_categories(&checks, &selected);

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

    /// Summarize how many of the run checks fall in each selected category.
    fn report_categories(checks: &[Box<dyn Check>], selected: &HashSet<Category>) {
        eprintln!("\n=== categories ===");
        for cat in Category::ALL {
            if !selected.contains(&cat) {
                continue;
            }
            let n = checks
                .iter()
                .filter(|c| c.categories().contains(&cat))
                .count();
            eprintln!("  {}: {n} check(s)", cat.label());
        }
    }

    /// Print every check grouped by category (name plus its categories) to
    /// stdout. A check appears under each category it belongs to.
    fn list_checks(checks: &[Box<dyn Check>]) {
        for cat in Category::ALL {
            let mut members = checks
                .iter()
                .filter(|c| c.categories().contains(&cat))
                .peekable();
            if members.peek().is_none() {
                continue;
            }
            println!("{}:", cat.label());
            for c in members {
                let labels = c
                    .categories()
                    .iter()
                    .map(|x| x.label())
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  {} [{labels}]", c.name());
            }
        }
    }
}

#[cfg(not(unix))]
pub fn execute(_workspace: &Path, options: ValidateOptions) -> TaskResult<()> {
    // The transport matrix relies on Unix-only APIs (POSIX ids, symlinks, device
    // nodes, ssh/daemon plumbing). Read each field so it is not reported as dead
    // on non-Unix targets (a `_` destructure discards without counting as a
    // read), then decline the command.
    let _ = (
        &options.transports,
        &options.flags,
        options.bench,
        options.bench_files,
        options.validation,
        options.edge_cases,
        options.security,
        options.wire,
        options.list,
        options.root,
        options.verbose,
    );
    let selected = selected_categories(&options);
    let _ = Category::ALL
        .iter()
        .filter(|c| selected.contains(c))
        .map(|c| c.label())
        .collect::<Vec<_>>();
    Err(crate::error::TaskError::Validation(
        "`cargo xtask validate` is only supported on Unix hosts".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(f: impl FnOnce(&mut ValidateOptions)) -> ValidateOptions {
        let mut o = ValidateOptions::default();
        f(&mut o);
        o
    }

    #[test]
    fn no_flags_defaults_to_validation() {
        let set = selected_categories(&ValidateOptions::default());
        assert_eq!(set, HashSet::from([Category::Validation]));
    }

    #[test]
    fn security_flag_selects_only_security() {
        let set = selected_categories(&opts(|o| o.security = true));
        assert_eq!(set, HashSet::from([Category::Security]));
    }

    #[test]
    fn edge_cases_flag_selects_edge_cases() {
        let set = selected_categories(&opts(|o| o.edge_cases = true));
        assert_eq!(set, HashSet::from([Category::EdgeCases]));
    }

    #[test]
    fn multiple_flags_union() {
        let set = selected_categories(&opts(|o| {
            o.validation = true;
            o.security = true;
            o.wire = true;
        }));
        assert_eq!(
            set,
            HashSet::from([Category::Validation, Category::Security, Category::Wire])
        );
    }

    #[test]
    fn category_labels_are_stable() {
        assert_eq!(Category::Validation.label(), "validation");
        assert_eq!(Category::EdgeCases.label(), "edge-cases");
        assert_eq!(Category::Security.label(), "security");
        assert_eq!(Category::Wire.label(), "wire");
    }

    #[cfg(unix)]
    #[test]
    fn all_checks_default_to_validation() {
        for check in checks::all() {
            assert_eq!(
                check.categories(),
                &[Category::Validation],
                "check `{}` should default to the Validation category",
                check.name()
            );
        }
    }
}
