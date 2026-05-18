#![no_main]

//! Differential fuzz target for the filter engine vs upstream rsync 3.4.2.
//!
//! Builds an oc-rsync [`FilterSet`] from a randomly generated rule chain and
//! evaluates it against a candidate path. The same rules and path are then
//! handed to a real upstream `rsync` binary running in `--dry-run --recursive
//! --verbose` mode, and the two verdicts are compared. A divergence panics,
//! which libFuzzer records as a crash artifact under `fuzz/artifacts/`.
//!
//! # Why differential?
//!
//! Reading the rsync man page is not enough. `exclude.c` has many quirks
//! around anchored vs unanchored patterns, directory-only rules, perishable
//! flags, and modifier interactions that are only visible when both engines
//! see the exact same byte sequence. This target catches the "we agree with
//! upstream's *text*" vs "we agree with upstream's *behaviour*" gap.
//!
//! # Throughput
//!
//! Each input spawns a child `rsync` process and waits for it to exit. On a
//! modern laptop expect roughly 200-500 exec/sec, which is two orders of
//! magnitude slower than an in-process fuzz target. This is acceptable: the
//! point of the harness is correctness, not raw coverage rate. Pair it with
//! `protocol_wire` and `simd_checksum_parity` for in-process throughput.
//!
//! # Upstream binary discovery
//!
//! The first hit wins:
//!
//! 1. `$OC_RSYNC_UPSTREAM_BIN` (absolute path, overrides everything)
//! 2. `target/interop/upstream-install/3.4.2/bin/rsync`
//! 3. `target/interop/upstream-install/3.4.1/bin/rsync`
//! 4. `/opt/homebrew/bin/rsync`
//! 5. `/usr/local/bin/rsync`
//! 6. `/usr/bin/rsync`
//!
//! When no binary is reachable the harness exits the iteration cleanly so
//! libFuzzer treats the input as benign rather than crashing. This keeps the
//! target usable on machines that do not have an upstream build available.
//!
//! # Running
//!
//! ```bash
//! cargo +nightly fuzz run filter_differential -- -max_total_time=60
//! ```
//!
//! The companion script `tools/ci/run_filter_fuzz.sh` wraps this command with
//! a configurable duration.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use filters::{FilterSet, parse_rules};

/// Upper bound on rules per input - keeps the rsync child argv small enough
/// to avoid hitting `ARG_MAX` on any supported platform.
const MAX_RULES: usize = 16;

/// Upper bound on pattern length per rule. Long patterns add no signal but
/// would slow exec rate.
const MAX_PATTERN_LEN: usize = 32;

/// Upper bound on the candidate path length.
const MAX_PATH_LEN: usize = 64;

/// Action selector for a generated filter rule. Maps to upstream rsync's
/// short-form rule prefixes (`+`, `-`, `P`, `R`, `H`, `S`).
#[derive(Arbitrary, Debug, Clone, Copy)]
enum RuleKind {
    Include,
    Exclude,
    Protect,
    Risk,
    Hide,
    Show,
}

impl RuleKind {
    fn prefix(self) -> char {
        match self {
            RuleKind::Include => '+',
            RuleKind::Exclude => '-',
            RuleKind::Protect => 'P',
            RuleKind::Risk => 'R',
            RuleKind::Hide => 'H',
            RuleKind::Show => 'S',
        }
    }
}

/// One generated rule: a kind and a pattern.
#[derive(Arbitrary, Debug)]
struct RuleSpec {
    kind: RuleKind,
    /// Raw bytes; we sanitise to printable ASCII below.
    pattern_bytes: Vec<u8>,
    /// Anchor the pattern with a leading `/`.
    anchored: bool,
    /// Make it a directory-only rule with a trailing `/`.
    dir_only: bool,
}

/// Top-level fuzz input.
#[derive(Arbitrary, Debug)]
struct Input {
    rules: Vec<RuleSpec>,
    path_bytes: Vec<u8>,
    is_dir: bool,
}

/// Restrict bytes to a small printable ASCII alphabet that is meaningful to
/// rsync's filter grammar (letters, digits, common glob and path glyphs).
/// We deliberately exclude whitespace, quotes, and shell metacharacters so
/// the rule survives both rsync's tokenizer and a process spawn that does
/// not invoke a shell.
fn sanitise_pattern(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().min(MAX_PATTERN_LEN));
    for &b in bytes.iter().take(MAX_PATTERN_LEN) {
        let c = match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => b as char,
            b'.' | b'_' | b'-' => b as char,
            b'*' | b'?' | b'/' | b'[' | b']' => b as char,
            _ => continue,
        };
        out.push(c);
    }
    // Collapse runs of slashes and strip a leading slash; the anchor flag is
    // the canonical way to anchor a pattern in this harness.
    let mut compact = String::with_capacity(out.len());
    let mut prev_slash = false;
    for c in out.chars() {
        if c == '/' {
            if prev_slash || compact.is_empty() {
                continue;
            }
            prev_slash = true;
        } else {
            prev_slash = false;
        }
        compact.push(c);
    }
    while compact.ends_with('/') {
        compact.pop();
    }
    if compact.is_empty() {
        compact.push('x');
    }
    compact
}

/// Sanitise a candidate path: printable ASCII, no `..`, no leading or
/// duplicated `/`, no empty segments. Returns a relative path with a single
/// or multi-segment form, but always rooted under the temp src dir.
fn sanitise_path(bytes: &[u8]) -> Option<String> {
    let mut out = String::with_capacity(bytes.len().min(MAX_PATH_LEN));
    for &b in bytes.iter().take(MAX_PATH_LEN) {
        let c = match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => b as char,
            b'.' | b'_' | b'-' => b as char,
            b'/' => b as char,
            _ => continue,
        };
        out.push(c);
    }
    // Collapse consecutive slashes and strip leading/trailing slashes.
    let cleaned: Vec<&str> = out.split('/').filter(|s| !s.is_empty()).collect();
    if cleaned.is_empty() {
        return None;
    }
    // Reject `..` and `.` segments outright - they short-circuit traversal.
    for seg in &cleaned {
        if *seg == "." || *seg == ".." {
            return None;
        }
    }
    Some(cleaned.join("/"))
}

/// Build a filter-rule line in rsync's short-form syntax.
///
/// Example outputs: `+ /foo`, `- bar/`, `P /data/`. The anchor and dir-only
/// markers are applied to the pattern itself, matching how a user would type
/// the rule into `--filter`.
fn render_rule_line(spec: &RuleSpec) -> String {
    let mut pattern = sanitise_pattern(&spec.pattern_bytes);
    if spec.anchored && !pattern.starts_with('/') {
        pattern.insert(0, '/');
    }
    if spec.dir_only && !pattern.ends_with('/') {
        pattern.push('/');
    }
    format!("{} {}", spec.kind.prefix(), pattern)
}

/// Cache the upstream rsync binary location across iterations.
fn upstream_binary() -> Option<&'static Path> {
    static UPSTREAM: OnceLock<Option<PathBuf>> = OnceLock::new();
    UPSTREAM
        .get_or_init(|| {
            if let Ok(env) = std::env::var("OC_RSYNC_UPSTREAM_BIN") {
                let p = PathBuf::from(env);
                if p.is_file() {
                    return Some(p);
                }
            }
            for candidate in [
                "target/interop/upstream-install/3.4.2/bin/rsync",
                "target/interop/upstream-install/3.4.1/bin/rsync",
                "/opt/homebrew/bin/rsync",
                "/usr/local/bin/rsync",
                "/usr/bin/rsync",
            ] {
                let p = PathBuf::from(candidate);
                if p.is_file() {
                    return Some(p);
                }
            }
            None
        })
        .as_deref()
}

/// Materialise the candidate path inside `root/src/` and ask upstream rsync
/// whether the leaf survives the filter chain.
///
/// Returns `Some(true)` for include, `Some(false)` for exclude, or `None` if
/// the probe could not be carried out (rsync failed, child process spawn
/// failed, etc.).
///
/// Mechanics:
///
/// - Build a src tree containing the candidate path (and any parent dirs).
/// - Run `rsync --dry-run --recursive --out-format=I:%n` against an empty
///   destination. Rsync emits one `I:<name>` line per transferred entry; an
///   excluded entry produces no line. Comparing for an exact match avoids
///   ambiguity with parent directories that are also reported.
fn upstream_verdict(
    rsync_bin: &Path,
    rule_lines: &[String],
    rel_path: &str,
    is_dir: bool,
) -> Option<bool> {
    let tmp = tempfile::tempdir().ok()?;
    let src_root = tmp.path().join("src");
    let dst_root = tmp.path().join("dst");
    std::fs::create_dir_all(&dst_root).ok()?;
    let leaf = src_root.join(rel_path);
    if is_dir {
        std::fs::create_dir_all(&leaf).ok()?;
    } else {
        if let Some(parent) = leaf.parent() {
            std::fs::create_dir_all(parent).ok()?;
        }
        std::fs::write(&leaf, b"x").ok()?;
    }

    let mut cmd = Command::new(rsync_bin);
    cmd.arg("--dry-run")
        .arg("--recursive")
        .arg("--verbose")
        .arg("--out-format=I:%n");
    for line in rule_lines {
        cmd.arg(format!("--filter={line}"));
    }
    // Trailing slash keeps rsync's path printing relative to src_root.
    let mut src_arg = src_root.into_os_string();
    src_arg.push("/");
    cmd.arg(src_arg);
    cmd.arg(&dst_root);

    let output = cmd.output().ok()?;
    if !output.status.success() {
        // Upstream rejected the rule chain - skip rather than treat as a
        // divergence. Rule grammar errors are reported by both engines as
        // parse failures and we already skip the input on the oc-rsync side
        // when `parse_rules` returns an error.
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Rsync emits directory names with a trailing slash in `%n`. Normalise
    // both sides by stripping it before comparing.
    let target = rel_path.trim_end_matches('/');
    let included = stdout.lines().any(|line| {
        let Some(name) = line.strip_prefix("I:") else {
            return false;
        };
        name.trim_end_matches('/').trim() == target
    });
    Some(included)
}

fn run_one(input: Input) {
    if input.rules.is_empty() || input.rules.len() > MAX_RULES {
        return;
    }
    let Some(rel_path) = sanitise_path(&input.path_bytes) else {
        return;
    };

    // Render the rule lines once - both sides see the exact same text.
    let rule_lines: Vec<String> = input.rules.iter().map(render_rule_line).collect();
    let merge_text = rule_lines.join("\n");

    // Parse on the oc-rsync side. A parse failure is a valid outcome; skip
    // such inputs since upstream would also reject them.
    let parsed = match parse_rules(&merge_text, Path::new("<fuzz>")) {
        Ok(rules) => rules,
        Err(_) => return,
    };
    let oc_set: FilterSet = match FilterSet::from_rules(parsed) {
        Ok(s) => s,
        Err(_) => return,
    };

    let oc_verdict = oc_set.allows(Path::new(&rel_path), input.is_dir);

    // The upstream probe only runs when a real rsync is reachable. Without
    // it, we still exercise the oc-rsync side, which guards against panics
    // in `parse_rules` and `FilterSet::from_rules` for arbitrary inputs.
    let Some(rsync_bin) = upstream_binary() else {
        return;
    };
    let Some(upstream_verdict) = upstream_verdict(rsync_bin, &rule_lines, &rel_path, input.is_dir)
    else {
        return;
    };

    assert_eq!(
        oc_verdict,
        upstream_verdict,
        "filter divergence: path={rel_path:?} is_dir={is_dir} oc={oc_verdict} upstream={upstream_verdict}\nrules:\n{rules}",
        is_dir = input.is_dir,
        rules = merge_text,
    );
}

fuzz_target!(|input: Input| {
    run_one(input);
});
