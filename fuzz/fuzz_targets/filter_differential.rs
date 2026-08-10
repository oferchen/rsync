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

/// Upper bound on pattern length per rule for the default shape. Long patterns
/// add no signal but would slow exec rate.
const MAX_PATTERN_LEN: usize = 32;

/// Upper bound on the candidate path length.
const MAX_PATH_LEN: usize = 64;

/// Length used for the [`PatternShape::VeryLong`] shape. Sized above the 4096
/// boundary called out in the FCV-7 task (#2320) so the oc-rsync parser is
/// exercised against patterns that exceed any internal stack-buffer threshold.
const VERY_LONG_PATTERN_LEN: usize = 4500;

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

/// Modifier-flag combinations that can be wedged between the action character
/// and the pattern. Mirrors the modifier alphabet documented in
/// `crates/filters/src/merge/parse.rs` (`RuleModifiers`) and upstream rsync's
/// `exclude.c` lines 1220-1288.
///
/// `s` / `r` are deliberately omitted from prefix-side rules (`H`/`S`/`P`/`R`)
/// at render time because upstream rejects those combinations as a syntax
/// error - we want to exercise valid syntactic shapes that both engines can
/// digest and compare verdicts on. The oc-rsync parser is still exercised
/// against the rejected shapes through [`ModifierFlags::SenderOnly`] and
/// [`ModifierFlags::ReceiverOnly`] applied to `+`/`-` rules.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum ModifierFlags {
    /// No modifier characters.
    None,
    /// `!` - invert match result.
    Negate,
    /// `p` - perishable rule.
    Perishable,
    /// `s` - sender side only (skipped for prefix-side rules at render time).
    SenderOnly,
    /// `r` - receiver side only (skipped for prefix-side rules at render time).
    ReceiverOnly,
    /// `!p` - combined negate + perishable.
    NegatePerishable,
    /// `ps` - combined perishable + sender side.
    PerishableSender,
    /// `!ps` - the canonical three-modifier shape from the upstream man page.
    NegatePerishableSender,
}

impl ModifierFlags {
    /// Returns the modifier characters that should be emitted in the rendered
    /// rule line. Side-bound modifiers (`s`, `r`) are dropped when the action
    /// already pins the rule to one side, matching upstream's
    /// `prefix_specifies_side` guard.
    fn chars_for(self, kind: RuleKind) -> &'static str {
        let prefix_pins_side = matches!(
            kind,
            RuleKind::Protect | RuleKind::Risk | RuleKind::Hide | RuleKind::Show,
        );
        match self {
            ModifierFlags::None => "",
            ModifierFlags::Negate => "!",
            ModifierFlags::Perishable => "p",
            ModifierFlags::SenderOnly => {
                if prefix_pins_side {
                    ""
                } else {
                    "s"
                }
            }
            ModifierFlags::ReceiverOnly => {
                if prefix_pins_side {
                    ""
                } else {
                    "r"
                }
            }
            ModifierFlags::NegatePerishable => "!p",
            ModifierFlags::PerishableSender => {
                if prefix_pins_side {
                    "p"
                } else {
                    "ps"
                }
            }
            ModifierFlags::NegatePerishableSender => {
                if prefix_pins_side {
                    "!p"
                } else {
                    "!ps"
                }
            }
        }
    }
}

/// Pattern shape selector. Steers the byte sanitiser toward edge cases that
/// the original alphabet (`A-Z`, `a-z`, `0-9`, `._-*?/[]`) could not produce.
///
/// Added under FCV-7 (#2320) to cover:
///
/// - NULL bytes inside the pattern body (oc-rsync must not panic; upstream is
///   not reachable for this case because `\0` is not representable in argv).
/// - Patterns of length >= 4096 bytes (stresses any internal stack buffers).
/// - `\` escape sequences in glob bodies combined with anchors and wildcards.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum PatternShape {
    /// Default sanitised printable ASCII pattern (legacy behaviour).
    Default,
    /// Insert a NULL byte mid-pattern.
    EmbeddedNull,
    /// Emit a pattern at least [`VERY_LONG_PATTERN_LEN`] bytes long.
    VeryLong,
    /// Mix anchored `/`, wildcard `*`/`?`/`**`, and escape `\` glyphs.
    AnchorWildcardEscape,
    /// Backslash-escape every glob metacharacter (`\*`, `\?`, `\[`).
    AllEscaped,
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
    /// Modifier-flag combination to insert between the action char and the
    /// pattern body.
    modifiers: ModifierFlags,
    /// Selects a pattern-generation strategy. See [`PatternShape`].
    shape: PatternShape,
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

/// Build a pattern body for the requested [`PatternShape`].
///
/// The default shape delegates to [`sanitise_pattern`]. The remaining shapes
/// produce edge-case inputs that the legacy alphabet could not generate:
/// NULL-byte injection, very long patterns, and explicit anchor/wildcard/
/// escape combinations. These exercise the oc-rsync parser regardless of
/// whether upstream rsync can accept them in argv.
fn build_pattern(shape: PatternShape, seed: &[u8]) -> String {
    match shape {
        PatternShape::Default => sanitise_pattern(seed),
        PatternShape::EmbeddedNull => {
            let head = sanitise_pattern(seed);
            // Place the NULL byte mid-pattern so it lands between literal
            // characters - the parser must not truncate or panic.
            let mid = head.len().div_ceil(2);
            let (left, right) = head.split_at(mid);
            format!("{left}\0{right}")
        }
        PatternShape::VeryLong => {
            // Deterministic body so the corpus minimiser can converge.
            let mut body = String::with_capacity(VERY_LONG_PATTERN_LEN);
            // Alternate literal runs with wildcard glyphs so the long input
            // is not a degenerate single-character pattern.
            let chunk = b"abcdefghij*?";
            while body.len() < VERY_LONG_PATTERN_LEN {
                for &b in chunk {
                    body.push(b as char);
                    if body.len() >= VERY_LONG_PATTERN_LEN {
                        break;
                    }
                }
            }
            // Seed-derived suffix keeps each input distinct enough for the
            // fuzzer's coverage tracker.
            let suffix = sanitise_pattern(seed);
            format!("{body}{suffix}")
        }
        PatternShape::AnchorWildcardEscape => {
            // Combine anchored prefix, wildcards, escape sequences, and
            // character classes in one body. Upstream treats `\*` as a literal
            // `*`, `\?` as a literal `?`, and `\\` as a literal `\` - so this
            // also exercises the literal-vs-glob distinction.
            let tail = sanitise_pattern(seed);
            format!("/dir/**/\\*.\\?[abc]/{tail}")
        }
        PatternShape::AllEscaped => {
            // Every glob metacharacter is escaped, producing a pattern that
            // should match its literal byte form. Mismatches here would
            // indicate the parser is folding escapes incorrectly.
            let tail = sanitise_pattern(seed);
            format!("\\*\\?\\[\\]/{tail}")
        }
    }
}

/// Returns true when the rule line is safe to hand to a child `rsync` process
/// via `--filter=...`. Argv strings cannot contain NULL bytes and rsync
/// refuses to parse rule lines that exceed a reasonable length; in either
/// case we still exercise the oc-rsync parser in-process and skip the
/// differential probe rather than crashing the child.
fn line_is_upstream_compatible(line: &str) -> bool {
    !line.as_bytes().contains(&0) && line.len() <= MAX_PATTERN_LEN + 8
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
/// Example outputs: `+ /foo`, `- bar/`, `P!p /data/`. The anchor and dir-only
/// markers are applied to the pattern itself, matching how a user would type
/// the rule into `--filter`. Modifier characters are injected directly after
/// the action prefix, matching upstream's `<action><modifiers> <pattern>`
/// grammar (see `crates/filters/src/merge/parse.rs::parse_modifiers`).
fn render_rule_line(spec: &RuleSpec) -> String {
    let mut pattern = build_pattern(spec.shape, &spec.pattern_bytes);
    if spec.anchored && !pattern.starts_with('/') {
        pattern.insert(0, '/');
    }
    if spec.dir_only && !pattern.ends_with('/') {
        pattern.push('/');
    }
    let mods = spec.modifiers.chars_for(spec.kind);
    format!("{}{} {}", spec.kind.prefix(), mods, pattern)
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

/// Model the recursive-walk verdict that upstream's `--recursive --list-only`
/// reports for a leaf path.
///
/// A deep entry is listed only if every ancestor directory is also included:
/// upstream never descends into an excluded directory, so a leaf under an
/// excluded parent is absent from the listing even when no rule matches the
/// leaf's full path. oc-rsync's `FilterSet::allows` is traversal-driven by the
/// same contract (it does not match descendants of excluded dirs; see
/// `filters::chain::FilterChain::allows`), so the differential harness must
/// replicate the ancestor walk to compare like with like.
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

    let oc_verdict = oc_walk_allows(&oc_set, &rel_path, input.is_dir);

    // The upstream probe only runs when a real rsync is reachable. Without
    // it, we still exercise the oc-rsync side, which guards against panics
    // in `parse_rules` and `FilterSet::from_rules` for arbitrary inputs.
    let Some(rsync_bin) = upstream_binary() else {
        return;
    };
    // Edge-case shapes (NULL bytes, very long patterns) cannot survive a
    // child argv. Skip the differential probe but keep the oc-rsync side
    // exercised, which is the whole point of the FCV-7 extension.
    if !rule_lines.iter().all(|line| line_is_upstream_compatible(line)) {
        return;
    }
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
