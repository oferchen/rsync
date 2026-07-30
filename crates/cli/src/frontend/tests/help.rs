use super::common::*;
use super::*;
use crate::frontend::defaults::SUPPORTED_OPTIONS_LIST;
use std::collections::BTreeSet;

#[test]
fn help_flag_renders_static_help_snapshot() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::Rsync);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_flag_uses_wrapped_program_name() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let expected = render_help(ProgramName::OcRsync);
    assert_eq!(stdout, expected.into_bytes());
}

#[test]
fn oc_help_mentions_daemon_option() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("valid UTF-8");
    // Help text should mention --daemon option
    assert!(rendered.contains("--daemon"));
    assert!(rendered.contains("Run as an rsync daemon"));
}

#[test]
fn oc_help_mentions_config_option() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(OC_RSYNC), OsStr::new("--help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("valid UTF-8");
    // Help text should mention --config option for daemon config
    assert!(rendered.contains("--config=FILE"));
}

#[test]
fn archive_help_matches_upstream_parenthetical() {
    // Upstream rsync 3.4.1 (help-rsync.h:10) renders the archive line as:
    //   --archive, -a            archive mode is -rlptgoD (no -A,-X,-U,-N,-H)
    // The parenthetical must match byte-for-byte for interop tooling that
    // grep `--help` output by upstream wording.
    let help = render_help(ProgramName::OcRsync);
    assert!(
        help.contains("archive mode is -rlptgoD (no -A,-X,-U,-N,-H)"),
        "rendered help missing upstream archive wording: {help}"
    );
}

#[test]
fn supported_options_list_mentions_all_help_flags() {
    let help = render_help(ProgramName::OcRsync);
    let options = collect_options(&help);

    for option in &options {
        assert!(
            SUPPORTED_OPTIONS_LIST.contains(option),
            "supported options list missing {option}"
        );
    }
}

/// Every oc-rsync-specific long flag. These must all live under the dedicated
/// "oc-rsync extensions" heading and never leak into the upstream-compatible
/// option groups.
const OC_EXTENSION_FLAGS: &[&str] = &[
    "--connect-program",
    "--tcp-fastopen",
    "--aes",
    "--no-aes",
    "--ssh-cipher",
    "--ssh-connect-timeout",
    "--ssh-keepalive",
    "--ssh-identity",
    "--ssh-no-agent",
    "--ssh-strict-host-key-checking",
    "--ssh-ipv6",
    "--ssh-port",
    "--jump-host",
    "--io-uring",
    "--no-io-uring",
    "--no-io-uring-sqpoll",
    "--io-uring-depth",
    "--simd",
    "--cow",
    "--no-cow",
    "--reflink",
    "--zero-copy",
    "--no-zero-copy",
    "--parallel-delta-scan",
    "--rayon-threads",
    "--checksum-threads",
    "--tokio-threads",
    "--apple-double-skip",
    "--sparse-detect",
    "--io-uring-status",
    "--lsm-status",
    "--spill-dir",
    "--spill-threshold-bytes",
    "--no-spill",
    "--password-command",
];

/// Total number of distinct option-definition lines carrying a long flag. The
/// reorganization only regroups lines, so this count (and the flag set below)
/// must stay constant: a drop shrinks it, an accidental duplicate collapses the
/// deduplicated set below it.
const TOTAL_LONG_FLAG_LINES: usize = 198;

/// Marker that opens the oc-rsync extensions section in the rendered help.
const EXTENSIONS_MARKER: &str = "oc-rsync extensions (not present in upstream rsync):";

/// Returns the long flags declared by an option-definition line (its header
/// only, not flags mentioned in the description text). Splitting on the first
/// double-space isolates the header from the description gap.
fn line_long_flags(line: &str) -> Vec<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('-') {
        return Vec::new();
    }
    let header = trimmed.split("  ").next().unwrap_or(trimmed);
    header
        .split([' ', ','])
        .filter(|tok| tok.starts_with("--"))
        .map(|tok| {
            let end = tok.find(['=', '/']).unwrap_or(tok.len());
            tok[..end].to_string()
        })
        .collect()
}

/// Primary long flag defined on each option line within `block`, in order.
fn primary_long_flags(block: &str) -> Vec<String> {
    block
        .lines()
        .filter_map(|line| line_long_flags(line).into_iter().next())
        .collect()
}

#[test]
fn oc_extensions_are_grouped_in_dedicated_section() {
    // WHY: oc-rsync exposes options that stock rsync does not understand. If they
    // are interleaved with the upstream-compatible options a user cannot tell,
    // from --help alone, which flags are portable to upstream rsync. Collecting
    // every oc-only flag under one clearly labelled heading keeps the
    // upstream-compatible surface a faithful, self-contained list.
    let help = render_help(ProgramName::OcRsync);
    let (upstream, extensions) = help
        .split_once(EXTENSIONS_MARKER)
        .expect("help must contain the oc-rsync extensions section");

    let upstream_flags: BTreeSet<String> = primary_long_flags(upstream).into_iter().collect();
    let extension_flags: BTreeSet<String> = primary_long_flags(extensions).into_iter().collect();

    for flag in OC_EXTENSION_FLAGS {
        assert!(
            extension_flags.contains(*flag),
            "oc-only flag {flag} is missing from the oc-rsync extensions section"
        );
        assert!(
            !upstream_flags.contains(*flag),
            "oc-only flag {flag} leaked into the upstream-compatible option groups"
        );
    }

    // The extensions section must contain oc-only flags and nothing else.
    let expected: BTreeSet<String> = OC_EXTENSION_FLAGS.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        extension_flags, expected,
        "the oc-rsync extensions section must define exactly the oc-only flags"
    );

    // A representative sample of upstream-compatible flags must stay above the
    // marker so the boundary never swallows portable options.
    const UPSTREAM_SAMPLE: &[&str] = &[
        "--archive",
        "--recursive",
        "--delete",
        "--compress",
        "--perms",
        "--times",
        "--owner",
        "--group",
        "--acls",
        "--xattrs",
        "--hard-links",
        "--copy-links",
        "--devices",
        "--partial",
        "--inplace",
        "--sparse",
        "--rsync-path",
        "--daemon",
        "--filter",
        "--exclude",
        "--backup",
        "--checksum",
        "--bwlimit",
        "--max-alloc",
        "--open-noatime",
    ];
    for flag in UPSTREAM_SAMPLE {
        assert!(
            upstream_flags.contains(*flag),
            "upstream flag {flag} must remain in the upstream-compatible groups"
        );
        assert!(
            !extension_flags.contains(*flag),
            "upstream flag {flag} must not appear under oc-rsync extensions"
        );
    }
}

#[test]
fn regrouping_preserves_the_full_option_set() {
    // WHY: the subject-based reorganization must be pure regrouping. No flag may
    // be dropped or duplicated when a line moves between sections; the inventory
    // is the invariant, only the ordering/headings change.
    let help = render_help(ProgramName::OcRsync);
    let primaries = primary_long_flags(&help);

    assert_eq!(
        primaries.len(),
        TOTAL_LONG_FLAG_LINES,
        "unexpected number of option-definition lines; a flag was added or dropped"
    );

    let distinct: BTreeSet<&String> = primaries.iter().collect();
    assert_eq!(
        distinct.len(),
        primaries.len(),
        "an option is defined on more than one line (duplicated during regrouping)"
    );

    // Partition invariant: the flag set splits cleanly into upstream + oc-only.
    let (upstream, extensions) = help.split_once(EXTENSIONS_MARKER).unwrap();
    let upstream_flags: BTreeSet<String> = primary_long_flags(upstream).into_iter().collect();
    let extension_flags: BTreeSet<String> = primary_long_flags(extensions).into_iter().collect();
    assert!(
        upstream_flags.is_disjoint(&extension_flags),
        "no flag may appear in both the upstream and the oc-rsync extensions groups"
    );
    assert_eq!(
        upstream_flags.len() + extension_flags.len(),
        distinct.len(),
        "the upstream and extension groups must together account for every flag"
    );
}

fn collect_options(text: &str) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '-' {
            match chars.peek() {
                Some('-') => {
                    chars.next();
                    let mut token = String::from("--");
                    while let Some(&next) = chars.peek() {
                        if next.is_ascii_alphanumeric() || next == '-' {
                            token.push(next);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if token.len() > 2 {
                        tokens.insert(token);
                    }
                }
                Some(next) if next.is_ascii_alphabetic() => {
                    let mut token = String::from("-");
                    token.push(*next);
                    chars.next();
                    tokens.insert(token);
                }
                _ => {}
            }
        }
    }
    tokens
}
