// Daemon `refuse options` directive matching.
//
// Implements the refuse-list evaluator that decides whether a client-requested
// option is rejected by a module's `refuse options` rule set. Mirrors upstream
// `clientserver.c` / `options.c` popt-based refuse semantics, including vital
// options that wildcards cannot touch, short/long option aliasing, and glob
// pattern matching.

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
    // upstream: options.c:975 marks the `log-format` long_options[] entry (the
    // deprecated alias of `out-format`) exact-match only - "aka out-format (NOT
    // log-file-format)". The wildcard-able `out-format` entry is left refusable,
    // so `log-format` is the vital name a `refuse options = *` cannot touch.
    "log-format",
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
///
/// Test-only: the live refusal point is `refused_client_arg`, applied to the
/// post-`@RSYNCD: OK` argv as upstream's `parse_arguments()` does. This helper
/// pins the per-option matcher both share.
#[cfg(test)]
fn refused_option<'a>(module: &ModuleDefinition, options: &'a [String]) -> Option<&'a str> {
    // No early-out on an empty refuse list: a daemon still refuses the default
    // device options (`copy-devices`/`write-devices`) even with no `refuse
    // options` line. upstream: options.c:984-987.
    options.iter().find_map(|candidate| {
        let canonical = canonical_option(candidate);
        let short = long_option_short_letter(&canonical);
        if is_option_refused(module, &canonical, short) {
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
/// One row of upstream's `long_options[]` as the refuse matcher needs it.
///
/// `long_options[]` is a single table that popt reads in both directions; oc
/// previously transcribed it into two independent `match` blocks that had
/// already drifted apart (46 arms one way, 41 the other). This is that one
/// table, so the two lookups below cannot disagree again.
///
/// upstream: `options.c:600-857` - the `shortName` and `longName` columns.
struct ShortOption {
    letter: char,
    /// `None` for rows whose `longName` is NULL, matched by letter only.
    long_name: Option<&'static str>,
}

/// Upstream's complete short-option column: all 51 letters.
///
/// Verified letter-for-letter against `options.c:600-856` (`long_options[]`,
/// terminator at :855): oc lacks none of upstream's letters and invents none.
/// `long_daemon_options[]` at :858 is deliberately out of scope - upstream's
/// own refuse scan walks only `long_options` (`options.c:921`).
///
/// THREE rows deliberately keep an oc-specific `long_name` where upstream has
/// NULL - `D` (options.c:670), `F` (:737) and `P` (:771). Each over-refuses
/// relative to upstream, which fails CLOSED, so they are an operator-visible
/// policy decision rather than a bug fix; see the per-row comments below.
const SHORT_OPTIONS: &[ShortOption] = &[
    ShortOption {
        letter: '@',
        long_name: Some("modify-window"),
    },
    ShortOption {
        letter: '0',
        long_name: Some("from0"),
    },
    ShortOption {
        letter: '4',
        long_name: Some("ipv4"),
    },
    ShortOption {
        letter: '6',
        long_name: Some("ipv6"),
    },
    ShortOption {
        letter: '8',
        long_name: Some("8-bit-output"),
    },
    ShortOption {
        letter: 'a',
        long_name: Some("archive"),
    },
    ShortOption {
        letter: 'A',
        long_name: Some("acls"),
    },
    ShortOption {
        letter: 'b',
        long_name: Some("backup"),
    },
    ShortOption {
        letter: 'B',
        long_name: Some("block-size"),
    },
    ShortOption {
        letter: 'c',
        long_name: Some("checksum"),
    },
    ShortOption {
        letter: 'C',
        long_name: Some("cvs-exclude"),
    },
    ShortOption {
        letter: 'd',
        long_name: Some("dirs"),
    },
    // upstream longName is NULL: `-D` is its own row meaning
    // `--devices --specials`. oc keeps the `devices` association, which
    // over-refuses (a `refuse options = devices` rule also blocks `-D`).
    // That fails CLOSED, so it is left alone here.
    ShortOption {
        letter: 'D',
        long_name: Some("devices"),
    },
    ShortOption {
        letter: 'e',
        long_name: Some("rsh"),
    },
    ShortOption {
        letter: 'E',
        long_name: Some("executability"),
    },
    ShortOption {
        letter: 'f',
        long_name: Some("filter"),
    },
    // upstream longName is NULL: `-F` is the repeated-filter shortcut. Same
    // fails-closed reasoning as `-D`.
    ShortOption {
        letter: 'F',
        long_name: Some("filter"),
    },
    ShortOption {
        letter: 'g',
        long_name: Some("group"),
    },
    ShortOption {
        letter: 'h',
        long_name: Some("human-readable"),
    },
    ShortOption {
        letter: 'H',
        long_name: Some("hard-links"),
    },
    ShortOption {
        letter: 'i',
        long_name: Some("itemize-changes"),
    },
    ShortOption {
        letter: 'I',
        long_name: Some("ignore-times"),
    },
    ShortOption {
        letter: 'J',
        long_name: Some("omit-link-times"),
    },
    ShortOption {
        letter: 'k',
        long_name: Some("copy-dirlinks"),
    },
    ShortOption {
        letter: 'K',
        long_name: Some("keep-dirlinks"),
    },
    ShortOption {
        letter: 'l',
        long_name: Some("links"),
    },
    ShortOption {
        letter: 'L',
        long_name: Some("copy-links"),
    },
    ShortOption {
        letter: 'm',
        long_name: Some("prune-empty-dirs"),
    },
    ShortOption {
        letter: 'M',
        long_name: Some("remote-option"),
    },
    ShortOption {
        letter: 'n',
        long_name: Some("dry-run"),
    },
    ShortOption {
        letter: 'N',
        long_name: Some("crtimes"),
    },
    ShortOption {
        letter: 'o',
        long_name: Some("owner"),
    },
    ShortOption {
        letter: 'O',
        long_name: Some("omit-dir-times"),
    },
    ShortOption {
        letter: 'p',
        long_name: Some("perms"),
    },
    // upstream longName is NULL: `-P` means `--partial --progress`.
    ShortOption {
        letter: 'P',
        long_name: Some("partial"),
    },
    ShortOption {
        letter: 'q',
        long_name: Some("quiet"),
    },
    ShortOption {
        letter: 'r',
        long_name: Some("recursive"),
    },
    ShortOption {
        letter: 'R',
        long_name: Some("relative"),
    },
    ShortOption {
        letter: 's',
        long_name: Some("secluded-args"),
    },
    ShortOption {
        letter: 'S',
        long_name: Some("sparse"),
    },
    ShortOption {
        letter: 't',
        long_name: Some("times"),
    },
    ShortOption {
        letter: 'T',
        long_name: Some("temp-dir"),
    },
    ShortOption {
        letter: 'u',
        long_name: Some("update"),
    },
    ShortOption {
        letter: 'U',
        long_name: Some("atimes"),
    },
    ShortOption {
        letter: 'v',
        long_name: Some("verbose"),
    },
    ShortOption {
        letter: 'V',
        long_name: Some("version"),
    },
    ShortOption {
        letter: 'W',
        long_name: Some("whole-file"),
    },
    ShortOption {
        letter: 'x',
        long_name: Some("one-file-system"),
    },
    ShortOption {
        letter: 'X',
        long_name: Some("xattrs"),
    },
    ShortOption {
        letter: 'y',
        long_name: Some("fuzzy"),
    },
    ShortOption {
        letter: 'z',
        long_name: Some("compress"),
    },
];

/// Looks up one short option, or `None` when the byte is not an option letter.
fn lookup_short(letter: char) -> Option<&'static ShortOption> {
    SHORT_OPTIONS.iter().find(|opt| opt.letter == letter)
}

/// Maps a canonical long-option name to its single-letter short form, when one
/// exists in upstream's `long_options[]` table.
///
/// Inverse of [`lookup_short`], derived from the same table. Used by
/// the refuse-list matcher so rules can reference either form (`refuse options
/// = verbose` and `= v` are equivalent).
///
/// upstream: `options.c:907` `parse_one_refuse_match()` - compares the rule
/// against BOTH the `longName` and the `shortName` of every entry.
fn long_option_short_letter(long_name: &str) -> Option<char> {
    SHORT_OPTIONS
        .iter()
        .find(|opt| opt.long_name == Some(long_name))
        .map(|opt| opt.letter)
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

    // upstream: options.c:2224-2250 - a `refuse options = delete` rule matches
    // the single `delete` popt entry, but the enforcement at options.c:2247 is
    // semantic: `if (refused_delete && (delete_mode || missing_args == 2))`.
    // Every delete-timing variant (`--delete-before/during/after/delay`),
    // `--delete-excluded`, `--del`, and `--delete-missing-args` sets
    // `delete_mode` (options.c:2224-2238), so refusing `delete` refuses them
    // all. The lexical per-arg scan below only matches e.g. `delete-during`
    // against a `delete*` glob, never the bare `delete` rule, so this semantic
    // pass catches the timing variants the client actually sends on the wire
    // (oc emits `--delete-during` for a plain `-a --delete`). The reported
    // option is always `--delete`, matching `create_refuse_error(refused_delete)`.
    if is_option_refused(module, "delete", None) {
        if client_args.iter().any(|arg| enables_delete_mode(arg)) {
            return Some("--delete".to_owned());
        }

        // upstream: options.c:2368-2376 - `--remove-source-files` inherits the
        // refusal of `delete`, but ONLY when this process is the sender:
        // `if (refused_delete && am_sender)`. On a pull the daemon is the
        // sender and would be deleting its OWN module contents, which is what
        // the rule exists to stop; on a push the deletions happen on the
        // client's side and upstream does not refuse them. The reported option
        // is `--delete`, from the same `create_refuse_error(refused_delete)`.
        //
        // Upstream's own comment pins the narrower half: the inference runs off
        // the refusal of "delete" only, never a `delete-FOO` option - and
        // `refused_delete` is set solely by the bare `delete` row
        // (options.c:1128), which is exactly what `is_option_refused(.., "delete")`
        // asks here.
        if daemon_is_sender(client_args)
            && client_args
                .iter()
                .any(|arg| is_long_option(arg, "remove-source-files"))
        {
            return Some("--delete".to_owned());
        }
    }

    for arg in client_args {
        let trimmed = arg.trim_start();
        if let Some(rest) = trimmed.strip_prefix("--") {
            let canonical = canonical_option(rest);
            if canonical.is_empty() {
                continue;
            }
            let short = long_option_short_letter(&canonical);
            if is_option_refused(module, &canonical, short) {
                return Some(format!("--{canonical}"));
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('-') {
            // Skip the dot-suffix capability string (e.g. `.LsfxCIvu`) and any
            // option-argument that follows a letter (e.g. `e.LsfxCIvu`).
            let letters = rest.split('.').next().unwrap_or("");
            for letter in letters.chars() {
                let Some(option) = lookup_short(letter) else {
                    // Not an option letter. Upstream is position-independent -
                    // popt marks a refused entry wherever it sits in the bundle
                    // (options.c:1040 rewrites `op->val`, options.c:1940
                    // returns it) - so an unrecognised byte must NOT end the
                    // scan. Breaking here let a client prefix its bundle with
                    // any non-letter and slip the rest past the refuse list
                    // entirely: `-4z` set compress with `refuse options =
                    // compress` in force, silently.
                    continue;
                };

                // NOTE: no option-argument (arity) handling here, deliberately.
                // Upstream can skip a value because ONE popt pass both parses
                // and marks refusals, so the two can never disagree. oc has two
                // readers of this bundle, and the one that actually APPLIES the
                // options - `transfer::flags::ParsedServerFlags::parse` - walks
                // every byte up to the `.` with no arity logic at all. Teaching
                // only this scanner about arity would make it skip letters the
                // decoder still acts on, which is a refusal BYPASS: `-B4096`
                // would hide a trailing flag that still took effect.
                //
                // The safe invariant while two readers exist: this scanner must
                // examine a SUPERSET of what the decoder acts on. Over-refusing
                // a byte that is really part of an argument fails CLOSED and is
                // acceptable; under-refusing is a security hole. Collapsing the
                // two readers onto one shared option table is task 138.

                // `long_name` is `None` only for rows upstream leaves NULL, in
                // which case the rule can only have named the bare letter.
                let refused = match option.long_name {
                    Some(long) => {
                        is_option_refused(module, long, Some(letter)).then(|| format!("--{long}"))
                    }
                    None => is_option_refused(module, &letter.to_string(), Some(letter))
                        .then(|| format!("-{letter}")),
                };
                if let Some(reported) = refused {
                    return Some(reported);
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
/// upstream: options.c:2224-2238 - `--delete`, `--del`, every
/// `--delete-WHEN` variant, and `--delete-excluded` all set `delete_mode`;
/// `--delete-missing-args` sets `missing_args = 2`. options.c:2247 then
/// refuses the transfer whenever `refused_delete` is set and any of those is
/// active. `--delete-missing-args` also needs the `missing_args == 2` guard
/// there, which matches this option once it has been requested.
/// Reports whether `arg` is the long option `--<long_name>`.
///
/// The `--` prefix is required: [`canonical_option`] strips leading dashes, so
/// matching on it alone would also fire on a bare transfer operand that happens
/// to be named like the option. Upstream's popt only ever treats the dashed
/// form as an option, and the operands sit after it in the same argv.
fn is_long_option(arg: &str, long_name: &str) -> bool {
    let trimmed = arg.trim();
    trimmed.starts_with("--") && canonical_option(trimmed) == long_name
}

/// Reports whether the daemon is the sender for this transfer, i.e. the client
/// is pulling from the module.
///
/// upstream: options.c:863 + :1524 - the client puts `--sender` in the server
/// argv and `OPT_SENDER` sets `am_sender = 1`. `sender` is one of the options a
/// module may never refuse (options.c:1048, mirrored in [`VITAL_OPTIONS`]), so
/// it is always present and never filtered out when the daemon is sending.
fn daemon_is_sender(client_args: &[String]) -> bool {
    client_args.iter().any(|arg| is_long_option(arg, "sender"))
}

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

/// Long-option spellings that name the same capability, so refusing any one of
/// them refuses all of them.
///
/// upstream: options.c `parse_one_refuse_match` - after an exact (non-wild)
/// rule matches a row it walks the whole table again and marks every other row
/// where `same_refuse_action(op, matched_op)` holds. That predicate folds two
/// rows together when both are constant assignments to the same destination
/// with the same resulting value (`refuse_const_assign`), or otherwise when
/// argInfo, val and destination all agree.
///
/// Upstream derives this from `long_options[]`, which oc does not have; the
/// groups below are the equivalence classes of that predicate, read off the
/// 3.5.0 table. Note that the folding is NOT "same argInfo": `--del` is
/// `POPT_ARG_NONE, &delete_during, 0` (assigning 1) while `--delete-during` is
/// `POPT_ARG_VAL, &delete_during, 1`, and `refuse_const_assign` normalises both
/// to (same destination, value 1). `--delete-delay` shares the destination but
/// stores 2, so it is deliberately a separate capability.
///
/// Like `VITAL_OPTIONS` and `DEFAULT_REFUSED_OPTIONS` above, this mirrors an
/// upstream table by hand and carries the same drift exposure they do.
const OPTION_ALIAS_GROUPS: &[&[&str]] = &[
    &["checksum-choice", "cc"],
    &["compress-choice", "zc"],
    &["compress-level", "zl"],
    &["compress-threads", "zt"],
    &["config", "dparam"],
    &["daemon", "detach", "no-detach"],
    &["del", "delete-during"],
    &["existing", "ignore-non-existing"],
    &["implied-dirs", "i-d"],
    &["inc-recursive", "i-r"],
    &["no-8-bit-output", "no-8"],
    &["no-acls", "no-A"],
    &["no-atimes", "no-U"],
    &["no-checksum", "no-c"],
    &["no-compress", "no-z"],
    &["no-crtimes", "no-N"],
    &["no-dirs", "no-d"],
    &["no-fuzzy", "no-y"],
    &["no-group", "no-g"],
    &["no-hard-links", "no-H"],
    &["no-human-readable", "no-h"],
    &["no-implied-dirs", "no-i-d"],
    &["no-inc-recursive", "no-i-r"],
    &["no-itemize-changes", "no-i"],
    &["no-links", "no-l"],
    &["no-omit-dir-times", "no-O"],
    &["no-omit-link-times", "no-J"],
    &["no-one-file-system", "no-x"],
    &["no-owner", "no-o"],
    &["no-perms", "no-p"],
    &["no-prune-empty-dirs", "no-m"],
    &["no-recursive", "no-r"],
    &["no-relative", "no-R"],
    &["no-secluded-args", "no-protect-args", "no-s"],
    &["no-sparse", "no-S"],
    &["no-times", "no-t"],
    &["no-verbose", "no-v"],
    &["no-whole-file", "no-W"],
    &["no-xattrs", "no-X"],
    &["old-dirs", "old-d"],
    &["out-format", "log-format"],
    &["secluded-args", "protect-args"],
    &["stop-after", "time-limit"],
];

/// Reports whether a non-wild refuse rule spelled `pattern` names the same
/// capability as the option spelled `long_name`.
///
/// Exact name first, then the alias groups, so an option outside every group
/// costs one comparison.
fn names_same_capability(pattern: &str, long_name: &str) -> bool {
    if pattern == long_name {
        return true;
    }
    OPTION_ALIAS_GROUPS
        .iter()
        .any(|group| group.contains(&pattern) && group.contains(&long_name))
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
fn is_option_refused(
    module: &ModuleDefinition,
    long_name: &str,
    short_letter: Option<char>,
) -> bool {
    let vital = is_option_vital(long_name, short_letter);
    // upstream: options.c:984-987 - a daemon seeds `copy-devices`/`write-devices`
    // as refused before applying the module's rules. Start from that default so
    // the loop below can only un-refuse them via an explicit negated exact match.
    let mut refused = DEFAULT_REFUSED_OPTIONS.contains(&long_name);
    // Compared in its ORIGINAL case: upstream passes `shortName` to `wildmatch`
    // verbatim (options.c:924). `-O` and `-o` are separate table rows
    // (omit-dir-times vs owner) and a rule naming one must not reach the other.
    let short_str = short_letter.map(|c| c.to_string());

    for rule in &module.refuse_options {
        let (negated, pattern_raw) = if let Some(rest) = rule.strip_prefix('!') {
            (true, rest)
        } else {
            (false, rule.as_str())
        };
        let mut pattern = canonical_option(pattern_raw);
        if pattern.is_empty() {
            continue;
        }

        // upstream: options.c:916 - `a` / `archive` rules expand to the
        // character class containing every short letter implied by `-a`.
        let mut is_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');
        if pattern == "a" || pattern == "archive" {
            pattern = "[ardlptgoD]".to_owned();
            is_glob = true;
        }

        // upstream: options.c:1050-1065 - vital options carry `descrip = "a="`
        // and `parse_one_refuse_match` only updates them when the rule is
        // exact, never wild. Mirror that here so administrators cannot wreck
        // the handshake with `refuse options = *`.
        if is_glob && vital {
            continue;
        }

        // upstream: options.c:921-924 - the rule is tried against both
        // `op->longName` and `op->shortName`, so `!verbose` and `!v` name the
        // same option. Both comparisons are case-SENSITIVE; that is what keeps
        // `[ardlptgoD]` matching `-D` while leaving `-d` alone.
        //
        // A non-wild rule names a CAPABILITY, not one spelling of it, so it
        // also covers every other spelling that sets the same thing - see
        // `names_same_capability`. Upstream applies that second pass only when
        // the match was exact (`if (!is_wild)`, options.c parse_one_refuse_match),
        // so the glob arm below deliberately does not consult the aliases.
        let matches = if is_glob {
            refuse_glob_match(&pattern, long_name)
                || short_str
                    .as_deref()
                    .is_some_and(|s| refuse_glob_match(&pattern, s))
        } else {
            names_same_capability(&pattern, long_name)
                || short_str.as_deref() == Some(pattern.as_str())
        };

        if matches {
            refused = !negated;
        }
    }

    // upstream: options.c:1005-1011 - once the module's own `refuse options`
    // rules have been applied, a daemon appends these refusals unconditionally,
    // so no `!log-file` / `!iconv` negation above can re-enable them:
    //   - `log-file*` (options.c:1010) refuses both `--log-file` and
    //     `--log-file-format`, keeping clients from redirecting the daemon's
    //     server-side logging.
    //   - `iconv` (options.c:1007-1008) is refused only when the module has no
    //     `charset` configured (`!*lp_charset(module_id)`).
    //   - `insecure-links` (options.c:1087, new in 3.5.0) is a LOCAL-ONLY
    //     opt-out, so a client must never be able to switch off the daemon's
    //     symlink confinement by sending it. The daemon's own opt-out is the
    //     `insecure links` module parameter, never this flag.
    if long_name.starts_with("log-file") {
        return true;
    }
    if long_name == "insecure-links" {
        return true;
    }
    if long_name == "iconv"
        && module
            .charset
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        return true;
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
        // No case-folded retry. Vitality is a property of one long_options[]
        // ROW, and the case-paired letters are different rows: `-n` (dry-run)
        // is vital, `-N` (crtimes) is not. Folding made `-N` inherit `-n`'s
        // immunity, so `refuse options = *` silently spared it - an
        // UNDER-refusal, the direction that fails open.
    }
    false
}

/// Returns whether an option is in the vital set that is immune to wildcards.
fn is_vital_option(canonical: &str) -> bool {
    VITAL_OPTIONS.contains(&canonical)
}

/// Matches a refuse-list pattern against a candidate option name.
///
/// Delegates to oc's `wildmatch`, which is the port of upstream's `dowild`
/// (`lib/wildmatch.c:78-296`). Upstream matches refuse rules with exactly that
/// function and nothing else - `options.c:921-924` calls
/// `wildmatch(ref, op->longName)` and `wildmatch(ref, shortName)`
/// unconditionally, for wild and non-wild rules alike.
///
/// This previously carried a second, hand-written glob that implemented `[...]`
/// as literal byte membership. That silently dropped two constructs `dowild`
/// supports:
///
/// - ranges, `[a-z]` (`wildmatch.c:156-166`)
/// - negation, `[!...]` (`wildmatch.c:139-143`)
///
/// and its doc claimed negation was unnecessary "since upstream's `[ardlptgoD]`
/// expansion never uses one" - which describes oc's own expansion, not what an
/// operator may write in `rsyncd.conf`. The direction of that error is what
/// made it serious: every other refuse divergence OVER-refuses and so fails
/// closed, but a range rule such as `refuse options = [A-Z]*` matched only the
/// literal bytes `A`, `-` and `Z`, so the options it was meant to block were
/// ACCEPTED. Delegating removes the divergence and the duplicate matcher.
fn refuse_glob_match(pattern: &str, text: &str) -> bool {
    filters::wildmatch(pattern.as_bytes(), text.as_bytes())
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
    // Case is PRESERVED. upstream matches refuse rules with `wildmatch`
    // (options.c:923-924), not `iwildmatch` - the case-folding variant exists
    // (lib/wildmatch.c:307-318, `force_lower_case = 1`) and is deliberately not
    // used here, and no `tolower`/`strcasecmp` appears anywhere in the refuse
    // path. Folding merged distinct options: `refuse options = O` also refused
    // `-o`, so a rule aimed at `--omit-dir-times` silently blocked `--owner`
    // and broke every `-a` transfer.
    token.to_owned()
}

#[cfg(test)]
mod capability_alias_tests {
    use super::*;

    fn module_refusing(rules: &[&str]) -> ModuleDefinition {
        ModuleDefinition {
            refuse_options: rules.iter().map(|rule| (*rule).to_owned()).collect(),
            ..ModuleDefinition::default()
        }
    }

    /// upstream: options.c `parse_one_refuse_match` - an exact match records
    /// `matched_op` and then marks every row `same_refuse_action` agrees with.
    /// `--del` (`POPT_ARG_NONE, &delete_during, 0`) and `--delete-during`
    /// (`POPT_ARG_VAL, &delete_during, 1`) both constant-assign 1 to the same
    /// destination, so they are one capability and a rule naming either refuses
    /// both. Both directions are asserted because the two table rows have
    /// different `argInfo`, so a fix that keyed on `argInfo` would pass one way
    /// and fail the other.
    #[test]
    fn an_exact_refuse_rule_covers_every_spelling_of_that_capability() {
        let canonical = module_refusing(&["delete-during"]);
        assert!(
            is_option_refused(&canonical, "del", None),
            "`refuse options = delete-during` must refuse --del: same destination, same stored value"
        );
        assert!(
            is_option_refused(&canonical, "delete-during", None),
            "a rule must still refuse the spelling it names"
        );

        let alias = module_refusing(&["del"]);
        assert!(
            is_option_refused(&alias, "delete-during", None),
            "`refuse options = del` must refuse --delete-during"
        );
        assert!(
            is_option_refused(&alias, "del", None),
            "a rule must still refuse the spelling it names"
        );
    }

    /// Non-vacuity companion: without it, an equivalence that simply grouped
    /// every `delete*` row together would satisfy the test above.
    ///
    /// `--delete-delay` shares the `&delete_during` destination but stores 2,
    /// and `--delete` writes a different variable entirely, so
    /// `same_refuse_action` separates both from `--del`.
    #[test]
    fn a_capability_rule_does_not_reach_a_row_that_stores_something_else() {
        let alias = module_refusing(&["del"]);
        assert!(
            !is_option_refused(&alias, "delete-delay", None),
            "--delete-delay assigns 2, not 1: a different capability"
        );
        assert!(
            !is_option_refused(&alias, "delete", None),
            "--delete assigns a different variable: a different capability"
        );
    }

    /// upstream gates the second pass on `if (!is_wild)`, so a glob marks only
    /// what it literally matches and never pulls in siblings.
    #[test]
    fn a_wildcard_rule_does_not_expand_to_aliases() {
        let wild = module_refusing(&["delete-d*"]);
        assert!(
            is_option_refused(&wild, "delete-during", None),
            "the glob still matches the name it spells"
        );
        assert!(
            !is_option_refused(&wild, "del", None),
            "a wildcard rule must not reach --del, which the glob does not match"
        );
    }

    /// Drift guard: oc's [`SHORT_OPTIONS`] letter column must equal upstream's
    /// complete `long_options[]` short-name set, letter for letter. The two
    /// derive from one popt table upstream but are transcribed by hand here, so
    /// an upstream bump that adds or drops a short option must redden this test
    /// rather than silently leaving oc's refuse matcher blind to a letter (or
    /// refusing one upstream no longer knows).
    ///
    /// The oracle below is every non-zero `shortName` in upstream
    /// `options.c:611-869` (`long_options[]`, opening brace at :609, terminator
    /// `{0,0,0,0, 0, 0, 0}` at :870), in source order. Three rows carry a NULL
    /// `longName` and are matched by letter only: `D` (options.c:679), `F`
    /// (:751) and `P` (:785). `long_daemon_options[]` (options.c:873) is out of
    /// scope: the refuse scan walks only `long_options[]`.
    ///
    /// upstream: options.c:609-871 - the single `long_options[]` popt table.
    #[test]
    fn short_option_letters_match_upstream_long_options_table() {
        use std::collections::BTreeSet;

        // Every non-zero shortName in options.c:611-869, in source order.
        const UPSTREAM_SHORT_LETTERS: &[char] = &[
            'V', // version          options.c:612
            'v', // verbose          options.c:613
            'q', // quiet            options.c:621
            'h', // human-readable   options.c:625
            'n', // dry-run          options.c:628
            'a', // archive          options.c:629
            'r', // recursive        options.c:630
            'd', // dirs             options.c:637
            'p', // perms            options.c:642
            'E', // executability    options.c:645
            'A', // acls             options.c:646
            'X', // xattrs           options.c:649
            't', // times            options.c:652
            'U', // atimes           options.c:655
            'N', // crtimes          options.c:660
            'O', // omit-dir-times   options.c:663
            'J', // omit-link-times  options.c:666
            '@', // modify-window    options.c:669
            'o', // owner            options.c:673
            'g', // group            options.c:676
            'D', // (NULL longName)  options.c:679
            'l', // links            options.c:691
            'L', // copy-links       options.c:694
            'k', // copy-dirlinks    options.c:701
            'K', // keep-dirlinks    options.c:702
            'H', // hard-links       options.c:703
            'R', // relative         options.c:706
            'I', // ignore-times     options.c:714
            'x', // one-file-system  options.c:716
            'u', // update           options.c:719
            'S', // sparse           options.c:726
            'F', // (NULL longName)  options.c:751
            'f', // filter           options.c:752
            'C', // cvs-exclude      options.c:757
            'W', // whole-file       options.c:758
            'c', // checksum         options.c:761
            'B', // block-size       options.c:766
            'y', // fuzzy            options.c:770
            'z', // compress         options.c:773
            'P', // (NULL longName)  options.c:785
            'm', // prune-empty-dirs options.c:793
            'i', // itemize-changes  options.c:800
            'b', // backup           options.c:805
            '0', // from0            options.c:814
            's', // secluded-args    options.c:818
            'e', // rsh              options.c:837
            'T', // temp-dir         options.c:839
            '4', // ipv4             options.c:842
            '6', // ipv6             options.c:843
            '8', // 8-bit-output     options.c:844
            'M', // remote-option    options.c:859
        ];

        let expected: BTreeSet<char> = UPSTREAM_SHORT_LETTERS.iter().copied().collect();
        let actual: BTreeSet<char> = SHORT_OPTIONS.iter().map(|opt| opt.letter).collect();

        // A collapsed duplicate would hide drift behind an equal-looking set,
        // so pin the source lists against their own de-duplicated form first.
        assert_eq!(
            UPSTREAM_SHORT_LETTERS.len(),
            expected.len(),
            "the transcribed upstream oracle contains a duplicate letter"
        );
        assert_eq!(
            SHORT_OPTIONS.len(),
            actual.len(),
            "oc SHORT_OPTIONS contains a duplicate letter"
        );

        let missing: Vec<char> = expected.difference(&actual).copied().collect();
        let extra: Vec<char> = actual.difference(&expected).copied().collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "oc SHORT_OPTIONS drifted from upstream long_options[] (options.c:609-871): \
             letters upstream has but oc lacks = {missing:?}; \
             letters oc refuses but upstream no longer knows = {extra:?}"
        );
    }
}

#[cfg(test)]
mod short_options_drift_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every short letter in upstream's `long_options[]` (`options.c:609-870`),
    /// paired with the `longName` on the same row. `None` marks the three rows
    /// upstream leaves NULL - `-D` (:679), `-F` (:747) and `-P` (:773) - which
    /// popt matches by letter only.
    ///
    /// This is the ground truth the [`SHORT_OPTIONS`] table must not drift from.
    /// It is transcribed by hand, so
    /// [`the_embedded_reference_matches_the_upstream_source`] re-derives it from
    /// the C source whenever that source is present and fails if the two differ,
    /// keeping this constant honest across upstream version bumps.
    const UPSTREAM_SHORT_LETTERS: &[(char, Option<&str>)] = &[
        ('V', Some("version")),
        ('v', Some("verbose")),
        ('q', Some("quiet")),
        ('h', Some("human-readable")),
        ('n', Some("dry-run")),
        ('a', Some("archive")),
        ('r', Some("recursive")),
        ('d', Some("dirs")),
        ('p', Some("perms")),
        ('E', Some("executability")),
        ('A', Some("acls")),
        ('X', Some("xattrs")),
        ('t', Some("times")),
        ('U', Some("atimes")),
        ('N', Some("crtimes")),
        ('O', Some("omit-dir-times")),
        ('J', Some("omit-link-times")),
        ('@', Some("modify-window")),
        ('o', Some("owner")),
        ('g', Some("group")),
        ('D', None),
        ('l', Some("links")),
        ('L', Some("copy-links")),
        ('k', Some("copy-dirlinks")),
        ('K', Some("keep-dirlinks")),
        ('H', Some("hard-links")),
        ('R', Some("relative")),
        ('I', Some("ignore-times")),
        ('x', Some("one-file-system")),
        ('u', Some("update")),
        ('S', Some("sparse")),
        ('F', None),
        ('f', Some("filter")),
        ('C', Some("cvs-exclude")),
        ('W', Some("whole-file")),
        ('c', Some("checksum")),
        ('B', Some("block-size")),
        ('y', Some("fuzzy")),
        ('z', Some("compress")),
        ('P', None),
        ('m', Some("prune-empty-dirs")),
        ('i', Some("itemize-changes")),
        ('b', Some("backup")),
        ('0', Some("from0")),
        ('s', Some("secluded-args")),
        ('e', Some("rsh")),
        ('T', Some("temp-dir")),
        ('4', Some("ipv4")),
        ('6', Some("ipv6")),
        ('8', Some("8-bit-output")),
        ('M', Some("remote-option")),
    ];

    /// The three letters upstream leaves NULL where oc assigns a `long_name`
    /// anyway, each over-refusing (fail CLOSED) as documented on the
    /// [`SHORT_OPTIONS`] rows. Any oc-specific `long_name` on a NULL upstream row
    /// OUTSIDE this set is a bug, not a policy choice, so the drift test rejects
    /// it.
    const DOCUMENTED_NULL_ROW_DIVERGENCES: &[(char, &str)] =
        &[('D', "devices"), ('F', "filter"), ('P', "partial")];

    /// The refuse matcher walks `SHORT_OPTIONS` and can only refuse a bundled
    /// letter it holds a row for, so a letter upstream ships but oc drops is a
    /// silent refusal BYPASS, and a letter oc invents over-refuses. Neither may
    /// happen unnoticed.
    #[test]
    fn short_options_covers_exactly_upstreams_short_letters() {
        let oc: std::collections::BTreeSet<char> =
            SHORT_OPTIONS.iter().map(|opt| opt.letter).collect();
        let upstream: std::collections::BTreeSet<char> = UPSTREAM_SHORT_LETTERS
            .iter()
            .map(|(letter, _)| *letter)
            .collect();

        let missing: Vec<char> = upstream.difference(&oc).copied().collect();
        let invented: Vec<char> = oc.difference(&upstream).copied().collect();
        assert!(
            missing.is_empty(),
            "SHORT_OPTIONS drops upstream short letters (refusal bypass): {missing:?}"
        );
        assert!(
            invented.is_empty(),
            "SHORT_OPTIONS invents letters upstream lacks (over-refusal): {invented:?}"
        );
        assert_eq!(
            SHORT_OPTIONS.len(),
            UPSTREAM_SHORT_LETTERS.len(),
            "a duplicate letter row would pass the set check but skew the count"
        );
    }

    /// A letter's `long_name` is the spelling a `refuse options` rule matches it
    /// by, so a wrong mapping silently refuses the wrong option. Every non-NULL
    /// upstream row must map identically; the only tolerated divergences are the
    /// three NULL rows oc deliberately names.
    #[test]
    fn short_options_long_names_match_upstream_except_documented_null_rows() {
        let divergences: BTreeMap<char, &str> =
            DOCUMENTED_NULL_ROW_DIVERGENCES.iter().copied().collect();

        for &(letter, upstream_long) in UPSTREAM_SHORT_LETTERS {
            let oc = lookup_short(letter)
                .unwrap_or_else(|| panic!("SHORT_OPTIONS is missing letter {letter:?}"));
            match upstream_long {
                Some(name) => assert_eq!(
                    oc.long_name,
                    Some(name),
                    "-{letter} must map to --{name} to match upstream long_options[]"
                ),
                None => {
                    let allowed = divergences.get(&letter).copied();
                    assert_eq!(
                        oc.long_name, allowed,
                        "-{letter} has a NULL longName upstream; oc may keep only its \
                         documented over-refusing alias, nothing else"
                    );
                }
            }
        }
    }

    /// Parses the `{"longName", 'x', ...}` / `{0, 'x', ...}` rows of upstream's
    /// `long_options[]`, returning `(shortName, longName)` for every row that
    /// carries a short letter. Rows whose short field is `0` (the `no-*` toggles
    /// and the daemon-mode options) carry no letter and are skipped, matching the
    /// subset [`SHORT_OPTIONS`] tracks.
    fn parse_upstream_long_options(src: &str) -> BTreeMap<char, Option<String>> {
        let start = src
            .find("static struct poptOption long_options[] = {")
            .expect("upstream long_options[] table not found");
        let mut out = BTreeMap::new();
        for line in src[start..].lines().skip(1) {
            let line = line.trim_start();
            let Some(body) = line.strip_prefix('{') else {
                continue;
            };
            // Terminator row `{0,0,0,0, 0, 0, 0}` ends the table.
            if body.trim_start().starts_with("0,0") {
                break;
            }
            let (long_name, rest) = if let Some(after) = body.strip_prefix('"') {
                let end = after.find('"').expect("unterminated longName string");
                (Some(after[..end].to_owned()), &after[end + 1..])
            } else {
                // A bare `0` longName (the three NULL rows).
                (None, body.trim_start().strip_prefix('0').unwrap_or(body))
            };
            // Advance to the short-letter field, immediately after the next `,`.
            let Some(comma) = rest.find(',') else {
                continue;
            };
            let short_field = rest[comma + 1..].trim_start();
            let mut chars = short_field.chars();
            if chars.next() != Some('\'') {
                // Short field is `0`: no bundled letter for this row.
                continue;
            }
            let letter = chars.next().expect("empty char literal in long_options[]");
            out.insert(letter, long_name);
        }
        out
    }

    /// Re-derives [`UPSTREAM_SHORT_LETTERS`] from the upstream C source and fails
    /// if the hand-transcribed constant has drifted from it. This is the single
    /// source of truth: the constant is only trustworthy because this test pins
    /// it to `options.c`. Skips (does not fail) when the interop source tree is
    /// absent, per the project rule that tests needing external resources degrade
    /// gracefully; the two constant-vs-`SHORT_OPTIONS` tests above still run.
    #[test]
    fn the_embedded_reference_matches_the_upstream_source() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/interop/upstream-src/rsync-3.5.0/options.c");
        let Ok(src) = std::fs::read_to_string(&path) else {
            eprintln!(
                "skipping: upstream options.c not present at {} (fetch via tools/ci/run_interop.sh)",
                path.display()
            );
            return;
        };

        let parsed = parse_upstream_long_options(&src);
        let expected: BTreeMap<char, Option<String>> = UPSTREAM_SHORT_LETTERS
            .iter()
            .map(|&(letter, long)| (letter, long.map(str::to_owned)))
            .collect();
        assert_eq!(
            parsed, expected,
            "UPSTREAM_SHORT_LETTERS has drifted from options.c long_options[]; \
             update the constant AND SHORT_OPTIONS to the new upstream table"
        );
    }
}

#[cfg(test)]
mod bundle_scan_tests {
    use super::*;

    fn module_refusing(rules: &[&str]) -> ModuleDefinition {
        ModuleDefinition {
            refuse_options: rules.iter().map(|rule| (*rule).to_owned()).collect(),
            ..ModuleDefinition::default()
        }
    }

    fn refused(rules: &[&str], args: &[&str]) -> Option<String> {
        let module = module_refusing(rules);
        let owned: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        refused_client_arg(&module, &owned)
    }

    /// The five short letters the pre-consolidation 46-arm map lacked - `@`, `0`,
    /// `4`, `6`, `8` (PR #7262 folded the drifted 46/41-arm maps into one 51-row
    /// table). Absent from that map, `lookup_short` returned `None` for them, so
    /// a bundled `-0`/`-4`/`-6`/`-8` slipped past the refuse list entirely. Each
    /// tuple is (bundled arg, refuse rule, reported option).
    #[test]
    fn the_five_restored_letters_are_refusable_in_a_bundle() {
        let cases = [
            ("-@", "modify-window", "--modify-window"),
            ("-0", "from0", "--from0"),
            ("-4", "ipv4", "--ipv4"),
            ("-6", "ipv6", "--ipv6"),
            ("-8", "8-bit-output", "--8-bit-output"),
        ];
        for (arg, rule, reported) in cases {
            assert_eq!(
                refused(&[rule], &[arg]),
                Some(reported.to_owned()),
                "`refuse options = {rule}` must reject a bundled {arg}"
            );
        }
    }

    /// A byte the table has no row for must NOT end the bundle scan: upstream is
    /// position-independent (popt marks a refused entry wherever it sits), so a
    /// leading unknown byte cannot be used to smuggle a refused letter behind it.
    /// `-9z` puts the unknown `9` before the refused `z`.
    ///
    /// upstream: options.c:1040 rewrites `op->val`, :1934 returns it - the mark
    /// is independent of position in the bundle.
    #[test]
    fn an_unknown_byte_does_not_end_the_bundle_scan() {
        assert_eq!(
            refused(&["compress"], &["-9z"]),
            Some("--compress".to_owned()),
            "an unknown leading byte must not hide the refused -z behind it"
        );
    }

    /// The dot-suffix capability string is skipped, but every byte before the `.`
    /// is examined: the scanner must match the SUPERSET of what the option
    /// decoder acts on, because oc's two readers of the bundle cannot be taught
    /// arity independently without opening a bypass. So the argument digits of an
    /// arg-taking flag (`-B4096`) are scanned as if they were option letters, and
    /// the `4` of `4096` trips a `refuse options = ipv4` rule.
    ///
    /// Over-refusing an argument byte fails CLOSED and is the deliberate cost of
    /// not modelling arity (see the `refused_client_arg` scan comment). This test
    /// pins that behaviour: an arity-aware scan that skipped `-B`'s value would
    /// return `None` here and reopen the bypass this guards.
    #[test]
    fn argument_bytes_are_scanned_as_option_letters() {
        assert_eq!(
            refused(&["ipv4"], &["-B4096"]),
            Some("--ipv4".to_owned()),
            "the `4` inside -B's argument must be scanned, proving no arity skipping"
        );
        assert_eq!(
            refused(&["compress"], &["-B4096z"]),
            Some("--compress".to_owned()),
            "a trailing -z after an arg-taking flag must still be refused"
        );
    }

    /// The dot-suffix (e.g. `-e.LsfxCIvu`, `.iLsfx`) is the capability string,
    /// not options, and is skipped - matching where the decoder stops. A rule
    /// naming a letter that appears only after the `.` must not fire.
    #[test]
    fn the_dot_capability_suffix_is_not_scanned() {
        assert_eq!(
            refused(&["xattrs"], &["-e.LsfxCIvu"]),
            None,
            "the X-less capability suffix carries no refusable options"
        );
        assert_eq!(
            refused(&["compress"], &["-e.iLsfxCIvuz"]),
            None,
            "a letter after the dot is part of the capability string, not an option"
        );
    }
}
