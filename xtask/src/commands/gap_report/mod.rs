#![deny(unsafe_code)]

//! `gap-report` command: receiver-option propagation coverage.
//!
//! This command institutionalizes a bug-discovery method. Upstream
//! `options.c` `server_options()` gates receiver-applied options behind
//! `if (am_sender)`, so those options are never sent over the wire on a pull.
//! The local client is the receiver and must carry each such option onto its
//! own `ServerConfig`. Any option whose `ServerConfig` field is never assigned
//! from `config` in a receiver builder is a latent propagation gap.
//!
//! The command reads the three receiver config builders (plus the shared flag
//! helper, which every builder invokes), statically checks each catalog option
//! for field propagation, and prints a coverage table. In `--check` mode it
//! compares the current gap set against a committed baseline and exits non-zero
//! when a new gap appears, matching the repository's other `--check` gates.

mod catalog;
mod scanner;

use crate::error::{TaskError, TaskResult};
use crate::util::read_file_with_context;
use catalog::{Builder, CATALOG, OptionSpec};
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::Path;

/// Workspace-relative path to the committed gap baseline.
const BASELINE_PATH: &str = "xtask/gap_report_baseline.txt";

/// Workspace-relative path to the shared flag helper every builder invokes.
const SHARED_FLAGS_PATH: &str = "crates/core/src/client/remote/flags.rs";

/// Arguments for the `gap-report` command.
#[derive(Debug, Default, Clone, Copy)]
pub struct GapReportArgs {
    /// Compare the current gap set against the committed baseline and fail on a
    /// new gap instead of printing the human report.
    pub check: bool,
}

impl From<crate::cli::GapReportArgs> for GapReportArgs {
    fn from(args: crate::cli::GapReportArgs) -> Self {
        GapReportArgs { check: args.check }
    }
}

/// A single gap: an option whose field is not propagated by a builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Gap {
    option: &'static str,
    builder: &'static str,
}

/// Verdict for one catalog option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Field propagated on every applicable builder.
    Ok,
    /// Field missing on at least one applicable builder.
    Gap,
    /// No `ServerConfig` field: propagated by another mechanism or unhandled.
    NoField,
}

/// Per-builder propagation cell in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cell {
    Propagated,
    Missing,
    NotApplicable,
    NoField,
}

impl Cell {
    const fn glyph(self) -> &'static str {
        match self {
            Cell::Propagated => "yes",
            Cell::Missing => "NO",
            Cell::NotApplicable => "n/a",
            Cell::NoField => "-",
        }
    }
}

/// Evaluated row for one catalog option.
struct Row {
    spec: &'static OptionSpec,
    cells: Vec<Cell>,
    verdict: Verdict,
}

/// Builder source, concatenated with the shared flag helper it invokes.
struct BuilderSources {
    contents: Vec<(Builder, String)>,
}

impl BuilderSources {
    /// Loads each receiver builder file joined with the shared flag helper.
    ///
    /// Every builder calls `apply_common_server_flags`, so a field assigned in
    /// the shared helper counts as propagated for all builders.
    fn load(workspace: &Path) -> TaskResult<Self> {
        let shared = read_file_with_context(&workspace.join(SHARED_FLAGS_PATH))?;
        let mut contents = Vec::with_capacity(Builder::ALL.len());
        for builder in Builder::ALL {
            let source = read_file_with_context(&workspace.join(builder.relative_path()))?;
            contents.push((builder, format!("{source}\n{shared}")));
        }
        Ok(Self { contents })
    }

    /// Returns the concatenated source for `builder`.
    fn source(&self, builder: Builder) -> &str {
        self.contents
            .iter()
            .find(|(candidate, _)| *candidate == builder)
            .map(|(_, source)| source.as_str())
            .unwrap_or_default()
    }
}

/// Executes the `gap-report` command.
pub fn execute(workspace: &Path, args: GapReportArgs) -> TaskResult<()> {
    let sources = BuilderSources::load(workspace)?;
    let rows: Vec<Row> = CATALOG
        .iter()
        .map(|spec| evaluate(spec, &sources))
        .collect();
    let gaps = current_gaps(&rows);

    if args.check {
        run_check(workspace, &gaps)
    } else {
        print!("{}", render_report(&rows, &gaps));
        Ok(())
    }
}

/// Evaluates one catalog option against every receiver builder.
fn evaluate(spec: &'static OptionSpec, sources: &BuilderSources) -> Row {
    let Some(field) = spec.server_field else {
        return Row {
            spec,
            cells: Builder::ALL.iter().map(|_| Cell::NoField).collect(),
            verdict: Verdict::NoField,
        };
    };

    let mut cells = Vec::with_capacity(Builder::ALL.len());
    let mut has_gap = false;
    for builder in Builder::ALL {
        let cell = if !spec.applicability.includes(builder) {
            Cell::NotApplicable
        } else if scanner::field_assigned_from_config(sources.source(builder), field) {
            Cell::Propagated
        } else {
            has_gap = true;
            Cell::Missing
        };
        cells.push(cell);
    }

    Row {
        spec,
        cells,
        verdict: if has_gap { Verdict::Gap } else { Verdict::Ok },
    }
}

/// Collects the current gap set from evaluated rows.
fn current_gaps(rows: &[Row]) -> BTreeSet<Gap> {
    let mut gaps = BTreeSet::new();
    for row in rows {
        for (builder, cell) in Builder::ALL.iter().zip(&row.cells) {
            if *cell == Cell::Missing {
                gaps.insert(Gap {
                    option: row.spec.upstream_name,
                    builder: builder.id(),
                });
            }
        }
    }
    gaps
}

/// Renders the human-readable coverage report.
fn render_report(rows: &[Row], gaps: &BTreeSet<Gap>) -> String {
    let mut out = String::new();
    out.push_str("Receiver-option propagation coverage (upstream rsync 3.4.4)\n");
    out.push_str(
        "Options gated by `if (am_sender)` in options.c server_options() are not\n\
         sent on a pull; the local receiver must carry them onto its ServerConfig.\n\n",
    );

    let header = format!(
        "{:<26} {:<16} {:<24} {:<5} {:<13} {:<7} {}\n",
        "OPTION", "OPTIONS.C", "CLIENTCONFIG GETTER", "ssh", "embedded-ssh", "daemon", "VERDICT"
    );
    out.push_str(&header);
    out.push_str(&"-".repeat(header.len().saturating_sub(1)));
    out.push('\n');

    for row in rows {
        let cells: Vec<&str> = row.cells.iter().map(|cell| cell.glyph()).collect();
        let _ = writeln!(
            out,
            "{:<26} {:<16} {:<24} {:<5} {:<13} {:<7} {}",
            row.spec.upstream_name,
            row.spec.options_c,
            row.spec.getter,
            cells[0],
            cells[1],
            cells[2],
            verdict_label(row.verdict),
        );
    }

    let ok = rows.iter().filter(|r| r.verdict == Verdict::Ok).count();
    let gap = rows.iter().filter(|r| r.verdict == Verdict::Gap).count();
    let no_field = rows
        .iter()
        .filter(|r| r.verdict == Verdict::NoField)
        .count();
    let _ = writeln!(
        out,
        "\n{} options: {ok} OK, {gap} GAP, {no_field} NO-FIELD",
        rows.len()
    );

    let annotated: Vec<&Row> = rows
        .iter()
        .filter(|row| row.verdict != Verdict::Ok)
        .collect();
    if !annotated.is_empty() {
        out.push_str("\nNotes (GAP and NO-FIELD options):\n");
        for row in annotated {
            let _ = writeln!(
                out,
                "  {} [{}]: {}",
                row.spec.upstream_name,
                verdict_label(row.verdict),
                row.spec.note,
            );
        }
    }

    out.push_str("\nCurrent gap set (baseline format):\n");
    if gaps.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for entry in gaps {
            let _ = writeln!(out, "  {} {}", entry.option, entry.builder);
        }
    }
    out
}

/// Returns the display label for a verdict.
const fn verdict_label(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Ok => "OK",
        Verdict::Gap => "GAP",
        Verdict::NoField => "NO-FIELD",
    }
}

/// Compares the current gap set against the committed baseline.
fn run_check(workspace: &Path, gaps: &BTreeSet<Gap>) -> TaskResult<()> {
    let baseline_path = workspace.join(BASELINE_PATH);
    let baseline_text = read_file_with_context(&baseline_path)?;
    let baseline = parse_baseline(&baseline_text);

    let new_gaps: Vec<&Gap> = gaps.iter().filter(|gap| !baseline.contains(*gap)).collect();
    let resolved: Vec<&Gap> = baseline.iter().filter(|gap| !gaps.contains(*gap)).collect();

    if !new_gaps.is_empty() {
        let mut message = String::from(
            "gap-report: new receiver-option propagation gap(s) detected.\n\
             An upstream am_sender-gated option is not carried onto a receiver\n\
             builder's ServerConfig. Fix the builder or, if intentional, update\n",
        );
        let _ = writeln!(message, "{BASELINE_PATH}:");
        for gap in new_gaps {
            let _ = writeln!(message, "  {} {}", gap.option, gap.builder);
        }
        return Err(TaskError::Validation(message.trim_end().to_string()));
    }

    if resolved.is_empty() {
        println!(
            "gap-report: no new gaps ({} baseline gap(s)).",
            baseline.len()
        );
    } else {
        println!(
            "gap-report: no new gaps; {} baseline gap(s) now resolved.",
            resolved.len()
        );
        println!("Tighten the guard by removing resolved entries from {BASELINE_PATH}:");
        for gap in resolved {
            println!("  {} {}", gap.option, gap.builder);
        }
    }
    Ok(())
}

/// Parses baseline lines into a gap set, ignoring comments and blank lines.
///
/// Each significant line is `<upstream-option> <builder-id>`. The tokens are
/// leaked to `'static` so parsed gaps compare directly against catalog-derived
/// gaps; the process is short-lived and the set is bounded by the catalog size.
fn parse_baseline(text: &str) -> BTreeSet<Gap> {
    let mut gaps = BTreeSet::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        if let (Some(option), Some(builder), None) = (parts.next(), parts.next(), parts.next()) {
            gaps.insert(Gap {
                option: Box::leak(option.to_owned().into_boxed_str()),
                builder: Box::leak(builder.to_owned().into_boxed_str()),
            });
        }
    }
    gaps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sources(embedded_missing: bool) -> BuilderSources {
        let common = "\
            server_config.flags.list_only = config.list_only();\n\
            server_config.write.delay_updates = config.delay_updates();\n\
            server_config.flags.numeric_ids = config.numeric_ids();\n";
        let embedded = if embedded_missing {
            "server_config.flags.numeric_ids = config.numeric_ids();\n"
        } else {
            common
        };
        BuilderSources {
            contents: vec![
                (Builder::Ssh, common.to_string()),
                (Builder::EmbeddedSsh, embedded.to_string()),
                (
                    Builder::Daemon,
                    format!("{common}server_config.write.fsync = config.fsync();\n"),
                ),
            ],
        }
    }

    fn spec(name: &'static str, field: &'static str) -> OptionSpec {
        OptionSpec {
            upstream_name: name,
            options_c: "options.c:1",
            getter: "getter",
            server_field: Some(field),
            applicability: catalog::Applicability::AllReceivers,
            note: "",
        }
    }

    #[test]
    fn fully_propagated_option_is_ok() {
        let sources = sample_sources(false);
        let leaked: &'static OptionSpec = Box::leak(Box::new(spec("--list-only", "list_only")));
        let row = evaluate(leaked, &sources);
        assert_eq!(row.verdict, Verdict::Ok);
        assert!(row.cells.iter().all(|c| *c == Cell::Propagated));
    }

    #[test]
    fn missing_embedded_field_is_a_gap() {
        let sources = sample_sources(true);
        let leaked: &'static OptionSpec = Box::leak(Box::new(spec("--list-only", "list_only")));
        let row = evaluate(leaked, &sources);
        assert_eq!(row.verdict, Verdict::Gap);
        assert_eq!(row.cells[1], Cell::Missing);
        let gaps = current_gaps(std::slice::from_ref(&row));
        assert!(gaps.contains(&Gap {
            option: "--list-only",
            builder: "embedded-ssh",
        }));
    }

    #[test]
    fn no_field_option_is_never_a_gap() {
        let sources = sample_sources(true);
        let no_field = OptionSpec {
            upstream_name: "--usermap",
            options_c: "options.c:1",
            getter: "user_mapping",
            server_field: None,
            applicability: catalog::Applicability::AllReceivers,
            note: "",
        };
        let leaked: &'static OptionSpec = Box::leak(Box::new(no_field));
        let row = evaluate(leaked, &sources);
        assert_eq!(row.verdict, Verdict::NoField);
        assert!(current_gaps(std::slice::from_ref(&row)).is_empty());
    }

    #[test]
    fn daemon_only_option_not_a_gap_on_ssh() {
        let sources = sample_sources(false);
        let mut fsync = spec("--fsync", "fsync");
        fsync.applicability = catalog::Applicability::DaemonOnly;
        let leaked: &'static OptionSpec = Box::leak(Box::new(fsync));
        let row = evaluate(leaked, &sources);
        assert_eq!(row.cells[0], Cell::NotApplicable);
        assert_eq!(row.cells[2], Cell::Propagated);
        assert_eq!(row.verdict, Verdict::Ok);
    }

    #[test]
    fn parse_baseline_ignores_comments_and_blanks() {
        let text = "# header\n\n--list-only embedded-ssh\n  --delay-updates embedded-ssh  \n";
        let parsed = parse_baseline(text);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.contains(&Gap {
            option: "--delay-updates",
            builder: "embedded-ssh",
        }));
    }

    #[test]
    fn parse_baseline_rejects_malformed_lines() {
        let parsed = parse_baseline("--only-one-token\n--a b c\n");
        assert!(parsed.is_empty());
    }

    #[test]
    fn render_report_lists_gap_set() {
        let sources = sample_sources(true);
        let leaked: &'static OptionSpec = Box::leak(Box::new(spec("--list-only", "list_only")));
        let row = evaluate(leaked, &sources);
        let gaps = current_gaps(std::slice::from_ref(&row));
        let report = render_report(std::slice::from_ref(&row), &gaps);
        assert!(report.contains("GAP"));
        assert!(report.contains("--list-only embedded-ssh"));
    }
}
