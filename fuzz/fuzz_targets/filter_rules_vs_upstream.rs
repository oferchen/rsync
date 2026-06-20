#![no_main]

//! Differential fuzz target #1365: oc-rsync filter chain vs upstream rsync's
//! own `--list-only` decision.
//!
//! Complements [`filter_differential`] (PR #4184) by probing upstream through
//! `--list-only` (the same surface that drives the man-page documentation) and
//! by exercising the rule-list-clear directive (`!`) that the original harness
//! deliberately omits. Both targets share the same oc-rsync side - the only
//! thing that varies is what subset of upstream's filter grammar is reached.
//!
//! # Probe shape
//!
//! For each random `(rules, path)` pair:
//!
//! 1. Build oc-rsync's `FilterSet` from the rule text and record
//!    [`FilterSet::allows`].
//! 2. Materialise `path` (file or directory) inside a temp `src/` tree.
//! 3. Run upstream as `rsync --dry-run --list-only --recursive --filter=...
//!    <src>/`. Upstream lists exactly the entries that survive the filter
//!    chain; an excluded path is absent from stdout.
//! 4. Compare the two verdicts. A divergence panics so libFuzzer keeps the
//!    minimised reproducer in `fuzz/artifacts/filter_rules_vs_upstream/`.
//!
//! # Cross-platform
//!
//! cargo-fuzz only ships sanitizer support for Linux and macOS. The fuzz body
//! is `cfg`-gated to those targets; on every other target the binary is a
//! no-op, and `tools/ci/run_filter_differential_fuzz.sh` exits cleanly when
//! `cargo fuzz` is missing.
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
//! When no upstream binary is reachable the iteration ends benignly - the
//! oc-rsync side still runs, which keeps the parser exercised on platforms
//! without an installed rsync.

use libfuzzer_sys::fuzz_target;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use arbitrary::Arbitrary;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use filters::{FilterSet, parse_rules};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::process::Command;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::OnceLock;

/// Upper bound on rules per input. Keeps the child argv well under `ARG_MAX`
/// on every supported platform.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_RULES: usize = 12;

/// Upper bound on pattern length per rule.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_PATTERN_LEN: usize = 32;

/// Upper bound on the candidate path length.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const MAX_PATH_LEN: usize = 64;

/// Action selector for a generated filter rule. Maps to upstream's short-form
/// prefixes documented in `rsync.1` under FILTER RULES.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Arbitrary, Debug, Clone, Copy)]
enum RuleKind {
    Include,
    Exclude,
    Protect,
    Risk,
    Hide,
    Show,
    /// List-clear directive `!`. Upstream resets the rule list at this point;
    /// oc-rsync honours the same semantics via `FilterAction::Clear`.
    Clear,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl RuleKind {
    fn prefix(self) -> &'static str {
        match self {
            RuleKind::Include => "+",
            RuleKind::Exclude => "-",
            RuleKind::Protect => "P",
            RuleKind::Risk => "R",
            RuleKind::Hide => "H",
            RuleKind::Show => "S",
            RuleKind::Clear => "!",
        }
    }
}

/// One generated rule: a kind, a pattern, and the anchor/dir-only markers.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Arbitrary, Debug)]
struct RuleSpec {
    kind: RuleKind,
    /// Raw bytes; sanitised to a small printable ASCII alphabet below.
    pattern_bytes: Vec<u8>,
    /// Anchor the pattern with a leading `/`.
    anchored: bool,
    /// Make it a directory-only rule with a trailing `/`.
    dir_only: bool,
}

/// Top-level fuzz input.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Arbitrary, Debug)]
struct Input {
    rules: Vec<RuleSpec>,
    path_bytes: Vec<u8>,
    is_dir: bool,
}

/// Restrict bytes to a small printable ASCII alphabet meaningful to rsync's
/// filter grammar (letters, digits, wildcards, character classes, path
/// separators). Whitespace, quotes, and shell metacharacters are dropped so
/// the rule survives both rsync's tokenizer and a process spawn that does
/// not invoke a shell.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

/// Sanitise a candidate path. Returns a relative path with non-empty segments
/// and without `.`/`..` components, or `None` if no valid path survives.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sanitise_path(bytes: &[u8]) -> Option<String> {
    let mut out = String::with_capacity(bytes.len().min(MAX_PATH_LEN));
    for &b in bytes.iter().take(MAX_PATH_LEN) {
        let c = match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' => b as char,
            b'.' | b'_' | b'-' | b'/' => b as char,
            _ => continue,
        };
        out.push(c);
    }
    let cleaned: Vec<&str> = out.split('/').filter(|s| !s.is_empty()).collect();
    if cleaned.is_empty() {
        return None;
    }
    for seg in &cleaned {
        if *seg == "." || *seg == ".." {
            return None;
        }
    }
    Some(cleaned.join("/"))
}

/// Build a filter-rule line in rsync's short-form syntax. The `!` directive
/// is emitted on its own; other rules carry a pattern body.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn render_rule_line(spec: &RuleSpec) -> String {
    if matches!(spec.kind, RuleKind::Clear) {
        return "!".to_string();
    }
    let mut pattern = sanitise_pattern(&spec.pattern_bytes);
    if spec.anchored && !pattern.starts_with('/') {
        pattern.insert(0, '/');
    }
    if spec.dir_only && !pattern.ends_with('/') {
        pattern.push('/');
    }
    format!("{} {}", spec.kind.prefix(), pattern)
}

/// Cache the upstream rsync binary path across iterations - filesystem stats
/// are surprisingly expensive at fuzzer exec rate.
#[cfg(any(target_os = "linux", target_os = "macos"))]
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

/// Materialise `rel_path` inside a temp `src/` tree and ask upstream rsync
/// whether the leaf survives the filter chain via `--list-only`.
///
/// Returns `Some(true)` for include, `Some(false)` for exclude, or `None` if
/// the probe could not be carried out (rsync rejected the rule, the spawn
/// failed, the tempdir could not be created, etc.).
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn upstream_verdict(
    rsync_bin: &Path,
    rule_lines: &[String],
    rel_path: &str,
    is_dir: bool,
) -> Option<bool> {
    let tmp = tempfile::tempdir().ok()?;
    let src_root = tmp.path().join("src");
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
    // `--list-only` runs the filter chain and prints surviving entries to
    // stdout without touching the filesystem - exactly what we need to read
    // upstream's verdict. `--no-h` keeps the output column layout stable.
    cmd.arg("--dry-run")
        .arg("--recursive")
        .arg("--list-only")
        .arg("--no-h");
    for line in rule_lines {
        cmd.arg(format!("--filter={line}"));
    }
    // Trailing slash keeps rsync printing entries relative to src_root.
    let mut src_arg = src_root.into_os_string();
    src_arg.push("/");
    cmd.arg(src_arg);

    let output = cmd.output().ok()?;
    if !output.status.success() {
        // Rule grammar errors are reported by both engines as parse failures;
        // we already skip on the oc-rsync side when `parse_rules` returns Err.
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let target = rel_path.trim_end_matches('/');
    // `--list-only` formats each surviving entry as
    //   <mode> <size> <date> <time> <name>
    // with `name` ending in `/` for directories. Trim and compare the final
    // whitespace-separated column.
    let included = stdout.lines().any(|line| {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return false;
        }
        let Some(name) = trimmed.rsplit_once(char::is_whitespace).map(|(_, n)| n) else {
            return false;
        };
        name.trim_end_matches('/') == target
    });
    Some(included)
}

/// Model the recursive-walk verdict that upstream's `--recursive --list-only`
/// reports for a leaf path.
///
/// A deep entry is listed only if every ancestor directory is also included:
/// upstream never descends into an excluded directory, so a leaf under an
/// excluded parent is absent from the listing even when no rule matches the
/// leaf's full path. oc-rsync's `FilterSet::allows` is traversal-driven by the
/// same contract (it does not match descendants of excluded dirs; see
/// `filters::chain::FilterChain::allows`), so this harness must replicate the
/// ancestor walk to compare like with like.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn oc_walk_allows(oc_set: &FilterSet, rel_path: &str, is_dir: bool) -> bool {
    let components: Vec<&str> = rel_path.split('/').filter(|s| !s.is_empty()).collect();
    let mut prefix = String::new();
    for (idx, component) in components.iter().enumerate() {
        if idx > 0 {
            prefix.push('/');
        }
        prefix.push_str(component);
        let is_leaf = idx + 1 == components.len();
        // Ancestor components are always directories; only the leaf carries the
        // caller's is_dir flag.
        let component_is_dir = if is_leaf { is_dir } else { true };
        if !oc_set.allows(Path::new(&prefix), component_is_dir) {
            return false;
        }
    }
    true
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_one(input: Input) {
    if input.rules.is_empty() || input.rules.len() > MAX_RULES {
        return;
    }
    let Some(rel_path) = sanitise_path(&input.path_bytes) else {
        return;
    };

    let rule_lines: Vec<String> = input.rules.iter().map(render_rule_line).collect();
    let merge_text = rule_lines.join("\n");

    let parsed = match parse_rules(&merge_text, Path::new("<fuzz>")) {
        Ok(rules) => rules,
        Err(_) => return,
    };
    let oc_set: FilterSet = match FilterSet::from_rules(parsed) {
        Ok(s) => s,
        Err(_) => return,
    };

    let oc_verdict = oc_walk_allows(&oc_set, &rel_path, input.is_dir);

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
        "filter divergence vs upstream: path={rel_path:?} is_dir={is_dir} oc={oc_verdict} upstream={upstream_verdict}\nrules:\n{rules}",
        is_dir = input.is_dir,
        rules = merge_text,
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fuzz_target!(|input: Input| {
    run_one(input);
});

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fuzz_target!(|_data: &[u8]| {
    // cargo-fuzz only ships sanitizer support for Linux and macOS; on every
    // other target this binary is a no-op.
});
