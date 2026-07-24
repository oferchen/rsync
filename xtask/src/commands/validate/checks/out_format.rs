//! `--out-format` stdout parity: oc renders each template byte-for-byte like
//! upstream rsync.
//!
//! Covers two directions. On a *pull* the client is the receiver and renders the
//! itemize/name templates (`%i`, `%n`, `%L`, `%o`). On a *push* the client is the
//! sender, which is the only side that expands `%f` to the full source path
//! (upstream `log.c` joins `F_PATHNAME` only when `am_sender`); the local-copy
//! path does not thread the source prefix, so `%f` is exercised over the remote
//! push transports only. Upstream is the ground truth for every template.

use std::path::Path;

use crate::commands::validate::support;
use crate::commands::validate::transport::{Transport, pull_into, push_into};
use crate::commands::validate::{Check, CheckOutcome, ValidateCtx};

/// The `--out-format` byte-parity check.
pub struct OutFormat;

/// Templates the receiving client renders on a pull. Chosen to be deterministic
/// across implementations: no per-file byte counts (`%b`/`%c`) or timestamps.
const PULL_TEMPLATES: &[&str] = &["%i %n%L", "%o %n"];

/// Templates the sending client renders on a push. `%f` is the sender-only full
/// source path this exercises; `%n` stays transfer-relative alongside it.
const PUSH_TEMPLATES: &[&str] = &["%f", "%i %f (%n)"];

/// Recursive, metadata-preserving, numeric ids - a stable itemize surface.
const BASE_FLAGS: &[&str] = &["-rlpt", "--numeric-ids"];

impl Check for OutFormat {
    fn name(&self) -> &'static str {
        "out_format"
    }

    fn run(&self, ctx: &ValidateCtx) -> Vec<CheckOutcome> {
        let root = ctx.work.join("out_format");
        let src = root.join("src");
        if let Err(e) = support::build_backdated_tree(&src) {
            return vec![CheckOutcome::skip(self.name(), "fixture", e)];
        }

        ctx.transports
            .iter()
            .flat_map(|&t| self.cell(ctx, t, &root))
            .collect()
    }
}

impl OutFormat {
    fn cell(&self, ctx: &ValidateCtx, transport: Transport, root: &Path) -> Vec<CheckOutcome> {
        let label = transport.label();
        if transport.needs_ssh() && !support::ssh_ready() {
            return vec![CheckOutcome::skip(
                self.name(),
                label,
                "no sshd on localhost:22",
            )];
        }
        let src = root.join("src");

        let mut outcomes: Vec<CheckOutcome> = PULL_TEMPLATES
            .iter()
            .map(|tmpl| self.compare(ctx, transport, &src, root, tmpl, Direction::Pull))
            .collect();

        // `%f` full-path is sender-only and unwired on the local-copy path, so
        // only the remote push transports carry the fix.
        if transport != Transport::Local {
            outcomes.extend(
                PUSH_TEMPLATES
                    .iter()
                    .map(|tmpl| self.compare(ctx, transport, &src, root, tmpl, Direction::Push)),
            );
        }
        outcomes
    }

    fn compare(
        &self,
        ctx: &ValidateCtx,
        transport: Transport,
        src: &Path,
        root: &Path,
        template: &str,
        direction: Direction,
    ) -> CheckOutcome {
        let label = format!("{}::{}::{template}", transport.label(), direction.label());
        let flags: Vec<String> = BASE_FLAGS
            .iter()
            .map(|s| (*s).to_string())
            .chain(std::iter::once(format!("--out-format={template}")))
            .collect();
        let oc_dst = root.join(format!("oc-{}-{}", transport.label(), direction.label()));
        let up_dst = root.join(format!("up-{}-{}", transport.label(), direction.label()));

        let run_one = |client: &Path, dst: &Path| match direction {
            Direction::Pull => {
                pull_into(transport, client, ctx.upstream, src, dst, &flags, ctx.work)
            }
            Direction::Push => {
                push_into(transport, client, ctx.upstream, src, dst, &flags, ctx.work)
            }
        };

        let up = match run_one(ctx.upstream, &up_dst) {
            Ok(out) if out.status.success() => out,
            other => {
                return crate::commands::validate::comparison::classify_failure(
                    self.name(),
                    label.as_str(),
                    "upstream",
                    other,
                );
            }
        };
        let oc = match run_one(ctx.oc, &oc_dst) {
            Ok(out) if out.status.success() => out,
            other => {
                return crate::commands::validate::comparison::classify_failure(
                    self.name(),
                    label.as_str(),
                    "oc",
                    other,
                );
            }
        };

        let up_text = String::from_utf8_lossy(&up.stdout);
        let oc_text = String::from_utf8_lossy(&oc.stdout);
        let up_lines = out_format_lines(&up_text);
        let oc_lines = out_format_lines(&oc_text);

        if ctx.verbose {
            eprintln!(
                "[out_format/{label}] oc {} lines, upstream {} lines",
                oc_lines.len(),
                up_lines.len()
            );
        }

        // Genuine-result guard: an empty render means the transfer produced no
        // per-file rows, so there is nothing to compare.
        if up_lines.is_empty() {
            return CheckOutcome::skip(self.name(), label.as_str(), "no upstream out-format rows");
        }
        if oc_lines != up_lines {
            let first_diff = oc_lines
                .iter()
                .zip(up_lines.iter())
                .find(|(o, u)| o != u)
                .map_or_else(
                    || {
                        format!(
                            "row count differs (oc {} vs {})",
                            oc_lines.len(),
                            up_lines.len()
                        )
                    },
                    |(o, u)| format!("oc {o:?} != upstream {u:?}"),
                );
            return CheckOutcome::fail(self.name(), label.as_str(), first_diff);
        }
        CheckOutcome::pass(self.name(), label.as_str())
    }
}

/// Transfer direction under test.
#[derive(Clone, Copy)]
enum Direction {
    Pull,
    Push,
}

impl Direction {
    const fn label(self) -> &'static str {
        match self {
            Direction::Pull => "pull",
            Direction::Push => "push",
        }
    }
}

/// Keep only the per-file `--out-format` rows, dropping the trailing transfer
/// summary (`sent ...`, `total size ...`, `speedup`) and blank lines so the
/// byte comparison is not perturbed by counters that legitimately differ.
fn out_format_lines(stdout: &str) -> Vec<&str> {
    stdout
        .lines()
        .map(str::trim_end)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("sent ")
                && !line.starts_with("total size ")
                && !line.contains("speedup is")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::out_format_lines;

    #[test]
    fn filters_summary_and_blank_lines() {
        let out = ">f+++++++++ a.txt\n\
                   cd+++++++++ sub/\n\
                   \n\
                   sent 100 bytes  received 20 bytes  240.00 bytes/sec\n\
                   total size is 12  speedup is 0.10\n";
        assert_eq!(
            out_format_lines(out),
            vec![">f+++++++++ a.txt", "cd+++++++++ sub/"]
        );
    }

    #[test]
    fn keeps_full_source_path_rows() {
        let out = "srcroot/docs/a.txt\nsrcroot/docs/sub/b.txt\n";
        assert_eq!(
            out_format_lines(out),
            vec!["srcroot/docs/a.txt", "srcroot/docs/sub/b.txt"]
        );
    }
}
