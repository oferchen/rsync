// Daemon `refuse options` directive matching.
//
// Implements the refuse-list evaluator that decides whether a client-requested
// option is rejected by a module's `refuse options` rule set. Mirrors upstream
// `clientserver.c` / `options.c` popt-based refuse semantics, including vital
// options that wildcards cannot touch, short/long option aliasing, and glob
// pattern matching.

fn parse_daemon_option(payload: &str) -> Option<&str> {
    let (keyword, remainder) = payload.split_once(char::is_whitespace)?;
    if !keyword.eq_ignore_ascii_case("OPTION") {
        return None;
    }

    let option = remainder.trim();
    if option.is_empty() {
        None
    } else {
        Some(option)
    }
}

/// Options that cannot be refused via wildcard-only patterns.
///
/// upstream: clientserver.c - `parse_refuse_options()` marks certain options as
/// "vital": they can only be refused by explicit name, not via `*` or other
/// glob wildcards. This prevents administrators from accidentally breaking the
/// protocol handshake by refusing all options with `*`.
const VITAL_OPTIONS: &[&str] = &[
    "server",
    "rsh",
    "e",
    "out-format",
    "sender",
    "dry-run",
    "n",
    "secluded-args",
    "s",
    "from0",
    "0",
    "iconv",
    "no-iconv",
    "checksum-seed",
    "copy-devices",
    "write-devices",
];

/// Options refused by default in daemon mode, overridable only by an explicit
/// negated exact match (e.g. `refuse options = !copy-devices`).
///
/// upstream: options.c:984-987 - when `am_daemon`, `parse_arguments` seeds the
/// refuse list with `copy-devices` and `write-devices` before applying the
/// module's `refuse options` rules, so a daemon rejects client device
/// read/write unless the module explicitly allows it. Both are also vital
/// (exact-match only, see `VITAL_OPTIONS`) so a `refuse options = *` wildcard
/// cannot silently re-enable them.
const DEFAULT_REFUSED_OPTIONS: &[&str] = &["copy-devices", "write-devices"];

/// Checks whether a client-requested option is refused by the module's refuse list.
///
/// The refuse list supports:
/// - Exact option names: `delete` refuses `--delete`
/// - Glob patterns: `delete*` refuses `--delete`, `--delete-before`, etc.
/// - Negation: `!delete-during` un-refuses a previously matched option
/// - Wildcard-all: `*` refuses everything except vital options
///
/// Vital options (e.g., `--server`, `--sender`, `--dry-run`) cannot be refused
/// by wildcard patterns and require explicit naming.
///
/// upstream: clientserver.c - `check_refuse_options()` with fnmatch semantics.
fn refused_option<'a>(module: &ModuleDefinition, options: &'a [String]) -> Option<&'a str> {
    // No early-out on an empty refuse list: a daemon still refuses the default
    // device options (`copy-devices`/`write-devices`) even with no `refuse
    // options` line. upstream: options.c:984-987.
    options.iter().find_map(|candidate| {
        let canonical = canonical_option(candidate);
        let short = long_option_short_letter(&canonical);
        if is_option_refused(&module.refuse_options, &canonical, short) {
            Some(candidate.as_str())
        } else {
            None
        }
    })
}

/// Maps a single short-option letter to its canonical long-name (lowercase).
///
/// Mirrors the `shortName` -> `longName` columns of upstream's `long_options[]`
/// table for the subset of options that ship as bundled short letters in the
/// daemon-mode argument string (e.g. `-vlogDtprez.iLsfxCIvu`). When no mapping
/// exists the literal letter is returned so wildcard-only refuse rules still
/// catch it.
///
/// upstream: options.c long_options[] - the canonical short/long pairing the
/// daemon's popt-based refuse check uses to compare against `refuse options`.
fn short_option_long_name(letter: char) -> &'static str {
    match letter {
        'v' => "verbose",
        'q' => "quiet",
        'h' => "human-readable",
        'n' => "dry-run",
        'a' => "archive",
        'r' => "recursive",
        'd' => "dirs",
        'p' => "perms",
        'E' => "executability",
        'A' => "acls",
        'X' => "xattrs",
        't' => "times",
        'U' => "atimes",
        'N' => "crtimes",
        'O' => "omit-dir-times",
        'J' => "omit-link-times",
        'o' => "owner",
        'g' => "group",
        'D' => "devices",
        'l' => "links",
        'L' => "copy-links",
        'k' => "copy-dirlinks",
        'K' => "keep-dirlinks",
        'H' => "hard-links",
        'R' => "relative",
        'I' => "ignore-times",
        'x' => "one-file-system",
        'u' => "update",
        'S' => "sparse",
        'F' => "filter",
        'C' => "cvs-exclude",
        'W' => "whole-file",
        'c' => "checksum",
        'y' => "fuzzy",
        'z' => "compress",
        'P' => "partial",
        'm' => "prune-empty-dirs",
        'i' => "itemize-changes",
        'b' => "backup",
        's' => "secluded-args",
        'V' => "version",
        'B' => "block-size",
        'T' => "temp-dir",
        'M' => "remote-option",
        'f' => "filter",
        'e' => "e",
        _ => "",
    }
}

/// Maps a canonical long-option name to its single-letter short form, when one
/// exists in upstream's `long_options[]` table.
///
/// Inverse of `short_option_long_name` for the subset of options that have a
/// short-letter alias. Used by the refuse-list matcher so rules can reference
/// either the long or short form (`!verbose` and `!v` are equivalent).
///
/// upstream: options.c:594-... - the `shortName` column on each `long_options[]`
/// entry; upstream's `parse_one_refuse_match` calls `wildmatch(ref, shortName)`
/// as a fallback when the long-name comparison fails.
fn long_option_short_letter(long_name: &str) -> Option<char> {
    match long_name {
        "verbose" => Some('v'),
        "quiet" => Some('q'),
        "human-readable" => Some('h'),
        "dry-run" => Some('n'),
        "archive" => Some('a'),
        "recursive" => Some('r'),
        "dirs" => Some('d'),
        "perms" => Some('p'),
        "executability" => Some('E'),
        "acls" => Some('A'),
        "xattrs" => Some('X'),
        "times" => Some('t'),
        "atimes" => Some('U'),
        "crtimes" => Some('N'),
        "omit-dir-times" => Some('O'),
        "omit-link-times" => Some('J'),
        "owner" => Some('o'),
        "group" => Some('g'),
        "devices" => Some('D'),
        "links" => Some('l'),
        "copy-links" => Some('L'),
        "copy-dirlinks" => Some('k'),
        "keep-dirlinks" => Some('K'),
        "hard-links" => Some('H'),
        "relative" => Some('R'),
        "ignore-times" => Some('I'),
        "one-file-system" => Some('x'),
        "update" => Some('u'),
        "sparse" => Some('S'),
        "cvs-exclude" => Some('C'),
        "whole-file" => Some('W'),
        "checksum" => Some('c'),
        "fuzzy" => Some('y'),
        "compress" => Some('z'),
        "partial" => Some('P'),
        "prune-empty-dirs" => Some('m'),
        "itemize-changes" => Some('i'),
        "backup" => Some('b'),
        "secluded-args" => Some('s'),
        "version" => Some('V'),
        "block-size" => Some('B'),
        "temp-dir" => Some('T'),
        "remote-option" => Some('M'),
        "filter" => Some('f'),
        _ => None,
    }
}

/// Checks whether any client argument is refused by the module's refuse list.
///
/// Expands bundled short options (e.g. `-vlogDtprez.iLsfxCIvu`) into their
/// long-name equivalents so a `refuse options = compress` rule rejects `-z`
/// inside a packed letter string the same way upstream rsync's popt-based
/// refuse check does.
///
/// Returns the long-name of the first refused option (formatted with the
/// `--` prefix to match the upstream `--<longname>` diagnostic) so callers
/// can include it verbatim in the error message.
///
/// upstream: clientserver.c - the daemon runs `parse_arguments()` on the
/// post-OK arg list; popt treats each bundled short letter as a separate
/// option and rejects any that the module's refuse list disabled.
fn refused_client_arg(module: &ModuleDefinition, client_args: &[String]) -> Option<String> {
    // No early-out on an empty refuse list: a daemon still refuses the default
    // device options (`copy-devices`/`write-devices`) even with no `refuse
    // options` line. upstream: options.c:984-987.

    // upstream: options.c:2215-2241 - a `refuse options = delete` rule matches
    // the single `delete` popt entry, but the enforcement at options.c:2238 is
    // semantic: `if (refused_delete && (delete_mode || missing_args == 2))`.
    // Every delete-timing variant (`--delete-before/during/after/delay`),
    // `--delete-excluded`, `--del`, and `--delete-missing-args` sets
    // `delete_mode` (options.c:2215-2229), so refusing `delete` refuses them
    // all. The lexical per-arg scan below only matches e.g. `delete-during`
    // against a `delete*` glob, never the bare `delete` rule, so this semantic
    // pass catches the timing variants the client actually sends on the wire
    // (oc emits `--delete-during` for a plain `-a --delete`). The reported
    // option is always `--delete`, matching `create_refuse_error(refused_delete)`.
    if is_option_refused(&module.refuse_options, "delete", None)
        && client_args.iter().any(|arg| enables_delete_mode(arg))
    {
        return Some("--delete".to_owned());
    }

    for arg in client_args {
        let trimmed = arg.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            let canonical = canonical_option(rest);
            if canonical.is_empty() {
                continue;
            }
            let short = long_option_short_letter(&canonical);
            if is_option_refused(&module.refuse_options, &canonical, short) {
                return Some(format!("--{canonical}"));
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('-') {
            // Skip the dot-suffix capability string (e.g. `.LsfxCIvu`) and any
            // option-argument that follows a letter (e.g. `e.LsfxCIvu`).
            let letters = rest.split('.').next().unwrap_or("");
            for letter in letters.chars() {
                if !letter.is_ascii_alphabetic() {
                    break;
                }
                let long = short_option_long_name(letter);
                let long_canonical = if long.is_empty() {
                    letter.to_ascii_lowercase().to_string()
                } else {
                    long.to_owned()
                };
                let short_letter = if long.is_empty() { None } else { Some(letter) };
                if is_option_refused(&module.refuse_options, &long_canonical, short_letter) {
                    return Some(if long.is_empty() {
                        format!("-{letter}")
                    } else {
                        format!("--{long}")
                    });
                }
            }
        }
    }
    None
}

/// Reports whether a client argument turns on the delete machinery, so a
/// `refuse options = delete` rule can reject it regardless of which timing
/// variant the client sent.
///
/// upstream: options.c:2215-2229 - `--delete`, `--del`, every
/// `--delete-WHEN` variant, and `--delete-excluded` all set `delete_mode`;
/// `--delete-missing-args` sets `missing_args = 2`. options.c:2238 then
/// refuses the transfer whenever `refused_delete` is set and any of those is
/// active. `--delete-missing-args` also needs the `missing_args == 2` guard
/// there, which matches this option once it has been requested.
fn enables_delete_mode(arg: &str) -> bool {
    let canonical = canonical_option(arg);
    matches!(
        canonical.as_str(),
        "del"
            | "delete"
            | "delete-before"
            | "delete-during"
            | "delete-delay"
            | "delete-after"
            | "delete-excluded"
            | "delete-missing-args"
    )
}

/// Evaluates a canonical option (long name + optional short letter) against an
/// ordered refuse list.
///
/// Mirrors upstream `set_refuse_options` / `parse_one_refuse_match`
/// (options.c:895): each rule is compared against BOTH the option's `longName`
/// and its `shortName`, and rules are applied in the order they appear so the
/// last match wins. A rule starting with `!` un-refuses a previously matched
/// option, enabling allow-list configurations like
/// `refuse options = * !verbose !archive` or pure `refuse options = !verbose`
/// inverses to function the same way `rsyncd.conf(5)` documents.
///
/// `a` and `archive` are special-cased to expand to the wildcard
/// `[ardlptgoD]` so they refuse every short letter implied by upstream's
/// `OPT_a` POPT alias, matching the `parse_one_refuse_match` rewrite at
/// options.c:904.
///
/// When a rule is a wildcard (`*`, `?`, `[`), it cannot affect vital options
/// (`--server`, `--sender`, `--dry-run`, `-e`, `-s`, ...). Non-wild rules
/// can refuse or un-refuse vital options when named explicitly.
fn is_option_refused(refuse_list: &[String], long_name: &str, short_letter: Option<char>) -> bool {
    let vital = is_option_vital(long_name, short_letter);
    // upstream: options.c:984-987 - a daemon seeds `copy-devices`/`write-devices`
    // as refused before applying the module's rules. Start from that default so
    // the loop below can only un-refuse them via an explicit negated exact match.
    let mut refused = DEFAULT_REFUSED_OPTIONS.contains(&long_name);
    let short_lower = short_letter.map(|c| c.to_ascii_lowercase().to_string());

    for rule in refuse_list {
        let (negated, pattern_raw) = if let Some(rest) = rule.strip_prefix('!') {
            (true, rest)
        } else {
            (false, rule.as_str())
        };
        let mut pattern = canonical_option(pattern_raw);
        if pattern.is_empty() {
            continue;
        }

        // upstream: options.c:904 - `a` / `archive` rules expand to the
        // character class containing every short letter implied by `-a`.
        let mut is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
        if pattern == "a" || pattern == "archive" {
            pattern = "[ardlptgoD]".to_owned();
            is_glob = true;
        }

        // upstream: options.c:953-968 - vital options carry `descrip = "a="`
        // and `parse_one_refuse_match` only updates them when the rule is
        // exact, never wild. Mirror that here so administrators cannot wreck
        // the handshake with `refuse options = *`.
        if is_glob && vital {
            continue;
        }

        // upstream: options.c:909-921 - the rule is tried against both
        // `op->longName` and `op->shortName` so `!verbose` and `!v` refer to
        // the same option. Glob patterns additionally need the original-case
        // short letter (so `[ardlptgoD]` matches `-D` via the upper-case `D`).
        let matches = if is_glob {
            refuse_glob_match(&pattern, long_name)
                || short_lower
                    .as_deref()
                    .is_some_and(|s| refuse_glob_match(&pattern, s))
                || short_letter.is_some_and(|c| {
                    let mut buf = [0u8; 4];
                    let original = c.encode_utf8(&mut buf);
                    refuse_glob_match(&pattern, original)
                })
        } else {
            pattern == long_name || short_lower.as_deref() == Some(pattern.as_str())
        };

        if matches {
            refused = !negated;
        }
    }
    refused
}

/// Returns true when either the long-form name or the short-letter form is in
/// the vital list, mirroring upstream's check of both `op->longName` and
/// `op->shortName` at options.c:953-965.
fn is_option_vital(long_name: &str, short_letter: Option<char>) -> bool {
    if is_vital_option(long_name) {
        return true;
    }
    if let Some(letter) = short_letter {
        let mut buf = [0u8; 4];
        let as_str = letter.encode_utf8(&mut buf);
        if is_vital_option(as_str) {
            return true;
        }
        let lower = letter.to_ascii_lowercase();
        if lower != letter {
            let lower_str = lower.encode_utf8(&mut buf);
            if is_vital_option(lower_str) {
                return true;
            }
        }
    }
    false
}

/// Returns whether an option is in the vital set that is immune to wildcards.
fn is_vital_option(canonical: &str) -> bool {
    VITAL_OPTIONS.contains(&canonical)
}

/// Matches a refuse-list glob pattern against a candidate option name.
///
/// Supports `*` (zero or more chars), `?` (one char), and `[...]` character
/// classes (case-sensitive, no negation `[!...]` since upstream's
/// `[ardlptgoD]` expansion never uses one). Falls back to the daemon's shared
/// `wildcard_match` when no character class is present so non-class globs keep
/// going through a single, well-tested matcher.
fn refuse_glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('[') {
        return wildcard_match(pattern, text);
    }

    let pat = pattern.as_bytes();
    let txt = text.as_bytes();
    let mut p = 0usize;
    let mut t = 0usize;
    let mut star_p: Option<usize> = None;
    let mut star_t = 0usize;

    while t < txt.len() {
        if p < pat.len() {
            match pat[p] {
                b'?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                b'*' => {
                    star_p = Some(p);
                    star_t = t;
                    p += 1;
                    continue;
                }
                b'[' => {
                    let class_end = pat[p + 1..].iter().position(|&b| b == b']');
                    if let Some(end) = class_end {
                        let class = &pat[p + 1..p + 1 + end];
                        if class.contains(&txt[t]) {
                            p += end + 2;
                            t += 1;
                            continue;
                        }
                    }
                    // Unterminated or non-matching class: fall through to backtrack.
                }
                ch if ch == txt[t] => {
                    p += 1;
                    t += 1;
                    continue;
                }
                _ => {}
            }
        }

        if let Some(sp) = star_p {
            p = sp + 1;
            star_t += 1;
            t = star_t;
        } else {
            return false;
        }
    }

    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// Extracts the canonical form of an option name for refuse-list matching.
///
/// Strips leading dashes, splits at whitespace or `=`, and lowercases.
fn canonical_option(text: &str) -> String {
    let token = text
        .trim()
        .trim_start_matches('-')
        .split([' ', '\t', '='])
        .next()
        .unwrap_or("");
    token.to_ascii_lowercase()
}
