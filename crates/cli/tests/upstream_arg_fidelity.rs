//! Permanent CLI-argument fidelity guard against upstream rsync 3.4.4.
//!
//! Every entry in this suite is derived from the upstream `poptOption`
//! table (`options.c:600` `long_options[]`), NOT from oc-rsync's current
//! behaviour. The upstream table is the contract: each long name (plus its
//! aliases), the associated short character, the argument type
//! (`POPT_ARG_NONE`/`POPT_ARG_VAL`/`POPT_ARG_INT`/`POPT_ARG_STRING`), and the
//! explicit `--no-*` negation entries. The suite asserts that oc-rsync's real
//! parser (`cli::test_utils::parse_args`, the same entry point the binary uses)
//! accepts and maps each of those forms identically.
//!
//! # How acceptance is determined
//!
//! oc-rsync hoists recognised options (and their space-separated values) ahead
//! of the positional operands before clap parses them; any *unrecognised*
//! option-looking token is instead left among the operands and is only rejected
//! later by `execution::operands::extract_operands`. Therefore "did the parser
//! recognise this option" cannot be answered by `parse_args(..).is_ok()` alone:
//! an unknown option parses successfully but leaks into `remainder`. Every
//! probe here appends two sentinel operands and asserts the parsed `remainder`
//! is *exactly* those two sentinels - proving the option (and its value) were
//! consumed by a real argument rather than mis-parsed as a path.
//!
//! # Divergences
//!
//! Where oc-rsync genuinely cannot yet satisfy the upstream contract, the exact
//! case is recorded in a `KNOWN_*` list (so the data-driven assertions stay
//! green while the gap is tracked) and is additionally pinned by a dedicated
//! `#[ignore]`d test that asserts the *upstream-correct* behaviour. When oc
//! gains the missing behaviour, the ignored test starts passing and the
//! matching `KNOWN_*` guard fails loudly, forcing removal of the stale entry.

use std::ffi::OsString;

use cli::test_utils::{ParsedArgs, parse_args};

const SRC: &str = "__oc_fidelity_src__";
const DST: &str = "__oc_fidelity_dst__";

/// Argument shape mirroring upstream `argInfo`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `POPT_ARG_NONE` / `POPT_ARG_VAL` / `POPT_BIT_SET` - takes no value.
    Flag,
    /// `POPT_ARG_INT` / `POPT_ARG_STRING` - consumes a value.
    Value,
}

/// One logical upstream option: its long name(s), short char, argument shape,
/// whether upstream provides a `--no-*` negation, and a benign sample value.
struct Opt {
    /// Long spellings, upstream-preferred first. Empty for short-only options
    /// (`-P`, `-D`, `-F`).
    long: &'static [&'static str],
    /// Upstream short character, if any.
    short: Option<char>,
    kind: Kind,
    /// Upstream defines an explicit `--no-<long[0]>` negation entry.
    negatable: bool,
    /// Sample value for `Kind::Value` options.
    sample: Option<&'static str>,
}

const fn flag(long: &'static [&'static str], short: Option<char>, negatable: bool) -> Opt {
    Opt {
        long,
        short,
        kind: Kind::Flag,
        negatable,
        sample: None,
    }
}

const fn value(
    long: &'static [&'static str],
    short: Option<char>,
    negatable: bool,
    sample: &'static str,
) -> Opt {
    Opt {
        long,
        short,
        kind: Kind::Value,
        negatable,
        sample: Some(sample),
    }
}

/// Exhaustive mirror of the user-facing entries in upstream `long_options[]`
/// (`options.c:600`). Daemon-transition and internal options (`--server`,
/// `--sender`, `--daemon`, `--config`, `--dparam`, `--detach`) are covered
/// separately in [`internal_and_daemon_options_are_recognized`].
const UPSTREAM_OPTS: &[Opt] = &[
    // --- general / output ---
    flag(&["help"], None, false),
    flag(&["version"], Some('V'), false),
    flag(&["verbose"], Some('v'), true),
    value(&["info"], None, false, "progress"),
    value(&["debug"], None, false, "deltasum"),
    value(&["stderr"], None, false, "errors"),
    flag(&["msgs2stderr"], None, true),
    flag(&["quiet"], Some('q'), false),
    flag(&["motd"], None, true),
    flag(&["stats"], None, false),
    flag(&["human-readable"], Some('h'), true),
    flag(&["dry-run"], Some('n'), false),
    flag(&["itemize-changes"], Some('i'), true),
    value(&["out-format", "log-format"], None, false, "%f"),
    value(&["log-file"], None, false, "/dev/null"),
    value(&["log-file-format"], None, false, "%f"),
    value(&["outbuf"], None, false, "none"),
    flag(&["8-bit-output"], Some('8'), true),
    // --- selection / recursion ---
    flag(&["archive"], Some('a'), false),
    flag(&["recursive"], Some('r'), true),
    flag(&["inc-recursive", "i-r"], None, true),
    flag(&["dirs"], Some('d'), true),
    flag(&["old-dirs", "old-d"], None, false),
    flag(&["relative"], Some('R'), true),
    flag(&["implied-dirs", "i-d"], None, true),
    flag(&["one-file-system"], Some('x'), true),
    flag(&["prune-empty-dirs"], Some('m'), true),
    flag(&["mkpath"], None, true),
    flag(&["existing", "ignore-non-existing"], None, false),
    flag(&["ignore-existing"], None, false),
    flag(&["update"], Some('u'), false),
    flag(&["ignore-times"], Some('I'), false),
    flag(&["size-only"], None, false),
    value(&["max-size"], None, false, "10M"),
    value(&["min-size"], None, false, "1K"),
    value(&["max-alloc"], None, false, "1G"),
    // --- filters ---
    value(&["filter"], Some('f'), false, "- foo"),
    value(&["exclude"], None, false, "*.tmp"),
    value(&["include"], None, false, "*.c"),
    value(&["exclude-from"], None, false, "/dev/null"),
    value(&["include-from"], None, false, "/dev/null"),
    flag(&["cvs-exclude"], Some('C'), false),
    value(&["files-from"], None, false, "/dev/null"),
    flag(&["from0"], Some('0'), true),
    // --- metadata preservation ---
    flag(&["perms"], Some('p'), true),
    flag(&["executability"], Some('E'), false),
    flag(&["acls"], Some('A'), true),
    flag(&["xattrs"], Some('X'), true),
    flag(&["times"], Some('t'), true),
    flag(&["atimes"], Some('U'), true),
    flag(&["open-noatime"], None, true),
    flag(&["crtimes"], Some('N'), true),
    flag(&["omit-dir-times"], Some('O'), true),
    flag(&["omit-link-times"], Some('J'), true),
    flag(&["owner"], Some('o'), true),
    flag(&["group"], Some('g'), true),
    flag(&["super"], None, true),
    flag(&["fake-super"], None, false),
    flag(&["numeric-ids"], None, true),
    value(&["usermap"], None, false, "1:2"),
    value(&["groupmap"], None, false, "1:2"),
    value(&["chown"], None, false, "root:root"),
    value(&["chmod"], None, false, "u+rwx"),
    value(&["copy-as"], None, false, "root"),
    value(&["modify-window"], Some('@'), false, "2"),
    // --- links / devices / specials ---
    flag(&["links"], Some('l'), true),
    flag(&["copy-links"], Some('L'), false),
    flag(&["copy-unsafe-links"], None, false),
    flag(&["safe-links"], None, false),
    flag(&["munge-links"], None, true),
    flag(&["copy-dirlinks"], Some('k'), false),
    flag(&["keep-dirlinks"], Some('K'), false),
    flag(&["hard-links"], Some('H'), true),
    flag(&["devices"], None, true),
    flag(&["copy-devices"], None, false),
    flag(&["write-devices"], None, true),
    flag(&["specials"], None, true),
    // --- delta / transfer ---
    flag(&["checksum"], Some('c'), true),
    value(&["checksum-choice", "cc"], None, false, "md5"),
    value(&["checksum-seed"], None, false, "1"),
    value(&["block-size"], Some('B'), false, "1024"),
    flag(&["whole-file"], Some('W'), true),
    flag(&["sparse"], Some('S'), true),
    flag(&["preallocate"], None, false),
    flag(&["inplace"], None, true),
    flag(&["append"], None, true),
    flag(&["append-verify"], None, false),
    flag(&["fuzzy"], Some('y'), true),
    value(&["compare-dest"], None, false, "/tmp"),
    value(&["copy-dest"], None, false, "/tmp"),
    value(&["link-dest"], None, false, "/tmp"),
    // --- compression ---
    flag(&["compress"], Some('z'), true),
    flag(&["old-compress"], None, false),
    flag(&["new-compress"], None, false),
    value(&["compress-choice", "zc"], None, false, "zlib"),
    value(&["compress-level", "zl"], None, false, "6"),
    value(&["compress-threads", "zt"], None, false, "2"),
    value(&["skip-compress"], None, false, "gz"),
    // --- deletion ---
    flag(&["del"], None, false),
    flag(&["delete"], None, false),
    flag(&["delete-before"], None, false),
    flag(&["delete-during"], None, false),
    flag(&["delete-delay"], None, false),
    flag(&["delete-after"], None, false),
    flag(&["delete-excluded"], None, false),
    flag(&["delete-missing-args"], None, false),
    flag(&["ignore-missing-args"], None, false),
    flag(&["remove-source-files"], None, false),
    flag(&["remove-sent-files"], None, false),
    flag(&["force"], None, true),
    flag(&["ignore-errors"], None, true),
    value(&["max-delete"], None, false, "100"),
    // --- backup ---
    flag(&["backup"], Some('b'), true),
    value(&["backup-dir"], None, false, "/tmp/bk"),
    value(&["suffix"], None, false, ".bak"),
    // --- partial / progress / batch ---
    flag(&["progress"], None, true),
    flag(&["partial"], None, true),
    value(&["partial-dir"], None, false, ".rsync-partial"),
    flag(&["delay-updates"], None, true),
    flag(&["list-only"], None, false),
    value(&["read-batch"], None, false, "/dev/null"),
    value(&["write-batch"], None, false, "/tmp/batch"),
    value(&["only-write-batch"], None, false, "/tmp/batch"),
    value(&["temp-dir"], Some('T'), false, "/tmp"),
    // --- bandwidth / timing ---
    value(&["bwlimit"], None, true, "1000"),
    value(&["timeout"], None, true, "60"),
    value(&["contimeout"], None, true, "60"),
    value(&["stop-after", "time-limit"], None, false, "10"),
    value(&["stop-at"], None, false, "2030-01-01T00:00"),
    // --- connection ---
    value(&["rsh"], Some('e'), false, "ssh"),
    value(&["rsync-path"], None, false, "rsync"),
    value(&["remote-option"], Some('M'), false, "--fake-super"),
    value(&["protocol"], None, false, "31"),
    value(&["address"], None, false, "0.0.0.0"),
    value(&["port"], None, false, "873"),
    value(&["sockopts"], None, false, "TCP_NODELAY"),
    value(&["password-file"], None, false, "/dev/null"),
    value(&["early-input"], None, false, "/dev/null"),
    flag(&["blocking-io"], None, true),
    flag(&["ipv4"], Some('4'), false),
    flag(&["ipv6"], Some('6'), false),
    // --- misc ---
    value(&["iconv"], None, true, "utf8"),
    flag(&["secluded-args", "protect-args"], Some('s'), true),
    flag(&["old-args"], None, true),
    flag(&["trust-sender"], None, false),
    flag(&["fsync"], None, false),
    flag(&["qsort"], None, false),
    // --- short-only combos ---
    flag(&[], Some('P'), false),
    flag(&[], Some('D'), false),
    flag(&[], Some('F'), false),
];

/// Short forms upstream accepts but oc-rsync currently rejects outright.
/// Keep in lockstep with the `#[ignore]`d trackers below.
const KNOWN_SHORT_REJECTED: &[(char, &str)] = &[
    // upstream options.c:660 `{"modify-window", '@', ...}` - oc wires only the
    // long form. Fix in flight.
    ('@', "modify-window short -@ not wired"),
    // upstream options.c:752 `{"block-size", 'B', ...}` - oc declares
    // `--block-size` long-only (no `.short('B')`).
    ('B', "block-size short -B not wired"),
];

/// Options that upstream (and oc) refuse without `-r`/`-d`: "--delete does not
/// work without --recursive (-r) or --dirs (-d)." (upstream options.c). Probing
/// these for parse-recognition therefore needs a recursion flag in context.
const NEEDS_RECURSION: &[&str] = &[
    "del",
    "delete",
    "delete-before",
    "delete-during",
    "delete-delay",
    "delete-after",
    "delete-excluded",
    "max-delete",
];

/// Prerequisite flags required for `opt` to satisfy upstream option
/// dependencies (so recognition probes exercise parsing, not cross-option
/// validation).
fn context_for(opt: &Opt) -> &'static [&'static str] {
    match opt.long.first() {
        Some(primary) if NEEDS_RECURSION.contains(primary) => &["-r"],
        _ => &[],
    }
}

/// Value options whose ATTACHED (`-Sval`) and EQUALS (`-S=val`) short forms oc
/// rejects for path-like values though upstream popt accepts them. oc's
/// `expand_short_options` omits these from its value-short set, so a value that
/// looks like an operand (`-T/tmp`, `-T=/tmp`) leaks into the operand list.
const KNOWN_SHORT_VALUE_FORM_REJECTED: &[&str] = &["temp-dir"];

/// Parses `tokens` with two trailing sentinel operands and confirms the option
/// (and any value) was consumed by a real argument, leaving only the sentinels
/// in `remainder`. Returns `Err` describing how the option diverged.
fn recognize(tokens: &[&str]) -> Result<ParsedArgs, String> {
    let mut argv: Vec<OsString> = Vec::with_capacity(tokens.len() + 3);
    argv.push(OsString::from("oc-rsync"));
    argv.extend(tokens.iter().map(OsString::from));
    argv.push(OsString::from(SRC));
    argv.push(OsString::from(DST));

    match parse_args(argv) {
        Ok(parsed) => {
            let expected = [OsString::from(SRC), OsString::from(DST)];
            if parsed.remainder == expected {
                Ok(parsed)
            } else {
                Err(format!(
                    "option leaked into operands; remainder = {:?}",
                    parsed.remainder
                ))
            }
        }
        Err(err) => Err(format!("clap rejected ({:?})", err.kind())),
    }
}

fn is_known_short_rejected(short: char) -> bool {
    KNOWN_SHORT_REJECTED.iter().any(|(c, _)| *c == short)
}

fn is_known_short_value_form_rejected(opt: &Opt) -> bool {
    opt.long
        .first()
        .is_some_and(|primary| KNOWN_SHORT_VALUE_FORM_REJECTED.contains(primary))
}

// --- 1. Every long form is accepted -----------------------------------------

#[test]
fn every_long_form_is_recognized() {
    let mut failures = Vec::new();
    for opt in UPSTREAM_OPTS {
        for &long in opt.long {
            let flag = format!("--{long}");
            let mut tokens: Vec<&str> = context_for(opt).to_vec();
            tokens.push(&flag);
            if opt.kind == Kind::Value {
                tokens.push(opt.sample.expect("value opt has sample"));
            }
            if let Err(why) = recognize(&tokens) {
                failures.push(format!("--{long}: {why}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "upstream long forms oc-rsync failed to accept:\n{}",
        failures.join("\n")
    );
}

// --- 2. Every short form is accepted (except tracked divergences) -----------

#[test]
fn every_short_form_is_recognized() {
    let mut failures = Vec::new();
    let mut stale_known = Vec::new();

    for opt in UPSTREAM_OPTS {
        let Some(short) = opt.short else { continue };
        let flag = format!("-{short}");
        let tokens: Vec<&str> = match opt.kind {
            Kind::Flag => vec![&flag],
            Kind::Value => vec![&flag, opt.sample.expect("value opt has sample")],
        };
        let result = recognize(&tokens);

        if is_known_short_rejected(short) {
            // Self-healing: if oc now accepts it, force the KNOWN list update.
            if result.is_ok() {
                stale_known.push(format!(
                    "-{short} is now accepted; remove it from KNOWN_SHORT_REJECTED \
                     and un-ignore its tracker test"
                ));
            }
        } else if let Err(why) = result {
            failures.push(format!("-{short}: {why}"));
        }
    }

    assert!(
        stale_known.is_empty(),
        "stale KNOWN_SHORT_REJECTED entries:\n{}",
        stale_known.join("\n")
    );
    assert!(
        failures.is_empty(),
        "upstream short forms oc-rsync failed to accept:\n{}",
        failures.join("\n")
    );
}

// --- 3. Value options accept both `--long=VALUE` and `--long VALUE` ----------

#[test]
fn value_options_accept_equals_and_space_forms() {
    let mut failures = Vec::new();
    for opt in UPSTREAM_OPTS {
        if opt.kind != Kind::Value {
            continue;
        }
        let sample = opt.sample.expect("value opt has sample");
        let context = context_for(opt);
        for &long in opt.long {
            let space = format!("--{long}");
            let mut space_tokens: Vec<&str> = context.to_vec();
            space_tokens.push(&space);
            space_tokens.push(sample);
            if let Err(why) = recognize(&space_tokens) {
                failures.push(format!("--{long} {sample}: {why}"));
            }
            let equals = format!("--{long}={sample}");
            let mut equals_tokens: Vec<&str> = context.to_vec();
            equals_tokens.push(&equals);
            if let Err(why) = recognize(&equals_tokens) {
                failures.push(format!("--{long}={sample}: {why}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "value options that rejected a =VALUE or space form:\n{}",
        failures.join("\n")
    );
}

// --- 3b. Short value options accept attached (`-Sval`) and equals (`-S=val`) -

#[test]
fn short_value_options_accept_attached_and_equals_forms() {
    let mut failures = Vec::new();
    let mut stale_known = Vec::new();

    for opt in UPSTREAM_OPTS {
        let Some(short) = opt.short else { continue };
        if opt.kind != Kind::Value {
            continue;
        }
        if is_known_short_rejected(short) {
            continue; // -B/-@ short is absent entirely; tracked elsewhere.
        }
        let sample = opt.sample.expect("value opt has sample");
        if sample.chars().any(char::is_whitespace) {
            continue; // attached form is ill-defined for values containing spaces.
        }
        let expect_ok = !is_known_short_value_form_rejected(opt);

        for form in [format!("-{short}{sample}"), format!("-{short}={sample}")] {
            let result = recognize(&[&form]);
            if expect_ok {
                if let Err(why) = result {
                    failures.push(format!("{form}: {why}"));
                }
            } else if result.is_ok() {
                stale_known.push(format!(
                    "{form} is now accepted; remove its long name from \
                     KNOWN_SHORT_VALUE_FORM_REJECTED and un-ignore its tracker"
                ));
            }
        }
    }

    assert!(
        stale_known.is_empty(),
        "stale KNOWN_SHORT_VALUE_FORM_REJECTED entries:\n{}",
        stale_known.join("\n")
    );
    assert!(
        failures.is_empty(),
        "short value forms oc-rsync failed to accept:\n{}",
        failures.join("\n")
    );
}

// --- 4. Every `--no-<long>` negation is accepted ----------------------------

#[test]
fn every_negation_is_recognized() {
    let mut failures = Vec::new();
    for opt in UPSTREAM_OPTS {
        if !opt.negatable {
            continue;
        }
        let primary = opt.long.first().expect("negatable opt has a long name");
        let negation = format!("--no-{primary}");
        if let Err(why) = recognize(&[&negation]) {
            failures.push(format!("--no-{primary}: {why}"));
        }
    }
    assert!(
        failures.is_empty(),
        "upstream negations oc-rsync failed to accept:\n{}",
        failures.join("\n")
    );
}

// --- 5. Short and long forms parse to the same settings ---------------------

#[test]
fn short_and_long_forms_agree() {
    let mut mismatches = Vec::new();
    for opt in UPSTREAM_OPTS {
        let Some(short) = opt.short else { continue };
        let Some(&primary) = opt.long.first() else {
            continue; // short-only (-P/-D/-F): nothing to compare against.
        };
        if is_known_short_rejected(short) {
            continue; // tracked separately; short form does not parse yet.
        }

        let short_flag = format!("-{short}");
        let long_flag = format!("--{primary}");
        let (short_tokens, long_tokens): (Vec<&str>, Vec<&str>) = match opt.kind {
            Kind::Flag => (vec![&short_flag], vec![&long_flag]),
            Kind::Value => {
                let sample = opt.sample.expect("value opt has sample");
                (vec![&short_flag, sample], vec![&long_flag, sample])
            }
        };

        match (recognize(&short_tokens), recognize(&long_tokens)) {
            (Ok(short_parsed), Ok(long_parsed)) => {
                if short_parsed != long_parsed {
                    mismatches.push(format!(
                        "-{short} and --{primary} produced different ParsedArgs"
                    ));
                }
            }
            (short_res, long_res) => mismatches.push(format!(
                "-{short}/--{primary} did not both parse: short={short_res:?} long={long_res:?}"
            )),
        }
    }
    assert!(
        mismatches.is_empty(),
        "short/long forms disagreed:\n{}",
        mismatches.join("\n")
    );
}

// --- 6. Short-flag bundling ------------------------------------------------

#[test]
fn short_flag_bundling_expands() {
    // Mixed bundle of independent flags.
    let avz = recognize(&["-avz"]).expect("-avz must parse");
    assert!(avz.archive, "-avz sets archive");
    assert!(avz.verbosity > 0, "-avz sets verbose");
    assert!(avz.compress, "-avz sets compress");

    // The canonical archive expansion bundle. Mirrors upstream `-a == -rlptgoD`
    // component-wise (archive resolution itself happens downstream).
    let expanded = recognize(&["-rlptgoD"]).expect("-rlptgoD must parse");
    assert!(expanded.recursive, "-r component");
    assert_eq!(expanded.links, Some(true), "-l component");
    assert_eq!(expanded.perms, Some(true), "-p component");
    assert_eq!(expanded.times, Some(true), "-t component");
    assert_eq!(expanded.group, Some(true), "-g component");
    assert_eq!(expanded.owner, Some(true), "-o component");
    assert_eq!(expanded.devices, Some(true), "-D enables devices");
    assert_eq!(expanded.specials, Some(true), "-D enables specials");

    // A trailing value-taking short may terminate a bundle and take the next
    // token as its argument (upstream popt semantics), e.g. `-vve ssh`.
    let bundle_value = recognize(&["-vve", "ssh"]).expect("-vve ssh must parse");
    assert!(bundle_value.verbosity >= 2, "-vv raises verbosity");
    assert_eq!(bundle_value.remote_shell, Some(OsString::from("ssh")));
}

// --- 7. Defaults match upstream --------------------------------------------

#[test]
fn defaults_match_upstream() {
    let base = parse_args(["oc-rsync", SRC, DST]).expect("bare invocation parses");

    // upstream options.c initialisers: everything preservation-related defaults
    // to "unset" (tri-state None) or off until an explicit flag or -a resolves
    // it downstream.
    assert!(!base.archive, "archive defaults off");
    assert!(!base.recursive, "recursive defaults off");
    assert_eq!(base.verbosity, 0, "verbosity defaults to 0");
    assert!(!base.dry_run, "dry-run defaults off");
    assert!(!base.compress, "compress defaults off");
    assert!(!base.from0, "from0 defaults off");
    assert!(!base.size_only, "size-only defaults off");
    assert_eq!(base.times, None, "times unset until -t/-a");
    assert_eq!(base.perms, None, "perms unset until -p/-a");
    assert_eq!(base.owner, None, "owner unset until -o/-a");
    assert_eq!(base.group, None, "group unset until -g/-a");
    assert_eq!(base.links, None, "links unset until -l/-a");
    assert_eq!(base.checksum, None, "checksum unset by default");
    assert_eq!(base.whole_file, None, "whole-file unset by default");
    assert_eq!(base.numeric_ids, None, "numeric-ids unset by default");
    assert_eq!(base.human_readable, None, "human-readable unset by default");

    // Sanity: a single -v yields verbosity 1, -vv yields 2 (upstream counts).
    assert_eq!(
        parse_args(["oc-rsync", "-v", SRC, DST]).unwrap().verbosity,
        1
    );
    assert_eq!(
        parse_args(["oc-rsync", "-vv", SRC, DST]).unwrap().verbosity,
        2
    );
}

// --- Internal / daemon-transition options -----------------------------------

#[test]
fn internal_and_daemon_options_are_recognized() {
    // `--server`/`--sender` are internal (set by remote invocation) but the
    // client parser must still accept them (upstream options.c:848-849).
    assert!(
        recognize(&["--server"]).is_ok(),
        "--server must be recognised"
    );
    assert!(
        recognize(&["--sender"]).is_ok(),
        "--sender must be recognised"
    );

    // Daemon-transition options (upstream options.c:851-855). These are defined
    // on the client command and parse into the daemon-related fields.
    let daemon = parse_args(["oc-rsync", "--daemon"]).expect("--daemon parses");
    assert!(daemon.daemon_mode, "--daemon sets daemon_mode");

    let detach = parse_args(["oc-rsync", "--daemon", "--no-detach"]).expect("--no-detach parses");
    assert_eq!(detach.detach, Some(false), "--no-detach sets detach=false");

    let cfg = parse_args(["oc-rsync", "--daemon", "--config", "/dev/null"])
        .expect("--config parses in daemon context");
    assert_eq!(cfg.config, Some(OsString::from("/dev/null")));

    let dparam = parse_args(["oc-rsync", "--daemon", "--dparam", "max connections=1"])
        .expect("--dparam parses in daemon context");
    assert_eq!(dparam.dparam, vec![OsString::from("max connections=1")]);
}

// --- Tracked divergences (upstream-correct contract; oc not yet compliant) ---
//
// Each test below asserts the UPSTREAM behaviour. It is `#[ignore]`d because
// oc-rsync currently diverges; removing the attribute must make it pass once
// the gap is closed. Do NOT weaken these assertions to make them green.

/// upstream options.c:660 `{"modify-window", '@', POPT_ARG_INT, ...}`.
#[test]
#[ignore = "oc divergence: -@ (modify-window short) not wired; use --modify-window"]
fn modify_window_short_at_is_accepted() {
    recognize(&["-@", "2"]).expect("-@ 2 must be accepted as --modify-window=2");
}

/// upstream options.c:752 `{"block-size", 'B', POPT_ARG_STRING, ...}`.
#[test]
#[ignore = "oc divergence: -B (block-size short) not wired; --block-size is long-only"]
fn block_size_short_b_is_accepted() {
    recognize(&["-B", "1024"]).expect("-B 1024 must be accepted as --block-size=1024");
}

/// popt accepts an attached value on any value-taking short option, including
/// path-like values (`-T/tmp`). oc's `expand_short_options` omits `T` from its
/// value-short set, so the token leaks into the operand list.
#[test]
#[ignore = "oc divergence: -T/PATH attached short-value rejected for path-like values"]
fn temp_dir_short_attached_value_is_accepted() {
    let parsed = recognize(&["-T/tmp"]).expect("-T/tmp must bind temp-dir");
    assert_eq!(
        parsed.temp_dir.as_deref(),
        Some(std::path::Path::new("/tmp"))
    );
}

/// popt accepts `-S=value` equals form on value-taking short options. oc
/// rejects `-T=/tmp` (only `-e=`, `-M=`, `-f=` are handled today).
#[test]
#[ignore = "oc divergence: -T=VALUE equals short-value form rejected"]
fn temp_dir_short_equals_value_is_accepted() {
    recognize(&["-T=/tmp"]).expect("-T=/tmp must bind temp-dir");
}

// --- Value-semantics divergences (validated below parse_args) ----------------
//
// These forms parse identically to upstream but are rejected/accepted at the
// value-validation layer, which `parse_args` does not exercise (it stores the
// raw string). They are pinned end-to-end against the built binary. The tests
// are `#[ignore]`d because oc currently diverges; each asserts the
// upstream-correct outcome. They run only under `--ignored` (never in the
// default CI test set) and require `cargo build --release --bin oc-rsync`.

/// Locates the `oc-rsync` binary (defined in the workspace-root package, so
/// `CARGO_BIN_EXE_oc-rsync` is not set for this crate's tests).
fn oc_binary() -> std::path::PathBuf {
    if let Some(explicit) = std::env::var_os("CARGO_BIN_EXE_oc-rsync") {
        return explicit.into();
    }
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/cli lives two levels below the workspace root");
    for profile in ["release", "debug"] {
        let candidate = root.join("target").join(profile).join("oc-rsync");
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("oc-rsync binary not found; run `cargo build --release --bin oc-rsync` first");
}

/// Runs `oc-rsync -n <args...> <tmp-src>/ <tmp-dst>/` over empty temp dirs so
/// only argument handling (not I/O) determines the exit status.
fn run_oc(args: &[&str]) -> std::process::ExitStatus {
    let src = tempfile::tempdir().expect("tempdir src");
    let dst = tempfile::tempdir().expect("tempdir dst");
    std::process::Command::new(oc_binary())
        .arg("-n")
        .args(args)
        .arg(format!("{}/", src.path().display()))
        .arg(format!("{}/", dst.path().display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("spawn oc-rsync")
}

/// upstream options.c:1129 - a trailing `+1`/`-1` adjusts a size argument
/// (`1K-1` == 1023). oc's size parser rejects the modifier.
#[test]
#[ignore = "oc divergence: --max-size/--min-size trailing +1/-1 modifier rejected"]
fn size_trailing_plus_minus_one_modifier_is_accepted() {
    assert!(
        run_oc(&["--max-size", "1K-1"]).success(),
        "upstream accepts --max-size=1K-1 (1023)"
    );
    assert!(
        run_oc(&["--max-size", "1K+1"]).success(),
        "upstream accepts --max-size=1K+1 (1025)"
    );
    assert!(
        run_oc(&["--min-size", "1K-1"]).success(),
        "upstream accepts --min-size=1K-1 (1023)"
    );
}

/// upstream options.c:1924 - `--stderr` accepts only `errors`, `all`, or
/// `client`; any other mode is a fatal error. oc silently accepts any string.
#[test]
#[ignore = "oc divergence: invalid --stderr mode not rejected"]
fn stderr_invalid_mode_is_rejected() {
    assert!(
        !run_oc(&["--stderr", "bogus"]).success(),
        "upstream rejects an unknown --stderr mode"
    );
    // The valid modes must still be accepted.
    for mode in ["errors", "all", "client"] {
        assert!(
            run_oc(&["--stderr", mode]).success(),
            "--stderr {mode} must be accepted"
        );
    }
}
