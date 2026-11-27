//! Unit tests for the popt-style server argv rewrite.

use super::long_options::LONG_OPTIONS;
use super::{
    ArgvRefusal, FILTER_OPTIONS, LongOption, is_two_arg_server_long_flag, normalize_server_argv,
};
use std::ffi::OsString;

fn argv(tokens: &[&str]) -> Vec<OsString> {
    tokens.iter().map(OsString::from).collect()
}

fn normalized(tokens: &[&str]) -> Vec<String> {
    normalize_server_argv(&argv(tokens))
        .expect("accepted")
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect()
}

/// Every short cluster, before `--server` or after it, joins one flag
/// string placed right after `--server`, as popt's single pass reads them.
#[test]
fn clusters_anywhere_join_one_flag_string() {
    // Ahead of `--server` a letter is parsed twice (options.c:1517-1524).
    assert_eq!(
        normalized(&["rsync", "-v", "--server", "-re.iLsfxCIvu", ".", "d/"]),
        ["rsync", "--server", "-vvre.iLsfxCIvu", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-r", ".", "-t", "d/"]),
        ["rsync", "--server", "-rt", ".", "d/"]
    );
}

/// The rewrite of an upstream server argv is the identity, so it is safe
/// to run twice (the secluded-args list goes through it again).
#[test]
fn upstream_server_argv_is_unchanged() {
    let upstream = [
        "rsync",
        "--server",
        "-logDtpre.iLsfxCIvu",
        "-B2048",
        "-@-1",
        "--delete",
        "--temp-dir",
        "/t",
        ".",
        "d/",
    ];
    assert_eq!(normalized(&upstream), upstream);
}

/// A value-taking letter ends the cluster; its value is the rest of the
/// token or the next token.
#[test]
fn value_options_take_the_rest_or_the_next_token() {
    assert_eq!(
        normalized(&["rsync", "-rM--x", "--server", ".", "d/"]),
        ["rsync", "--server", "-rr", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "-B", "2048", "-T", "/t", "--server", ".", "d/"]),
        ["rsync", "-B2048", "--temp-dir=/t", "--server", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-re", ".iLs", ".", "d/"]),
        ["rsync", "--server", "-re.iLs", ".", "d/"]
    );
}

/// Filter options join their values behind the flag string in argv order;
/// those ahead of `--server` count twice (options.c:1517-1524 restarts the
/// pass without clearing `filter_list` or `F_option_cnt`).
#[test]
fn filter_options_are_joined_and_doubled_ahead_of_server() {
    assert_eq!(
        normalized(&["rsync", "-f", "- *.tmp", "--server", "-r", ".", "d/"]),
        [
            "rsync",
            "--server",
            "-r",
            "--filter=- *.tmp",
            "--filter=- *.tmp",
            ".",
            "d/"
        ]
    );
    assert_eq!(
        normalized(&[
            "rsync",
            "--server",
            "-rF",
            "--exclude",
            "b",
            "--include=a",
            ".",
            "d/"
        ]),
        [
            "rsync",
            "--server",
            "-r",
            "--filter=: /.rsync-filter",
            "--exclude=b",
            "--include=a",
            ".",
            "d/"
        ]
    );
    // One `-F` ahead of `--server` is two, and a third adds nothing.
    assert_eq!(
        normalized(&["rsync", "-F", "--server", "-rF", ".", "d/"]),
        [
            "rsync",
            "--server",
            "-r",
            "--filter=: /.rsync-filter",
            "--filter=- .rsync-filter",
            ".",
            "d/"
        ]
    );
    let once = normalized(&["rsync", "-f", "- x", "--server", "-r", ".", "d/"]);
    let once: Vec<OsString> = once.into_iter().map(OsString::from).collect();
    assert_eq!(normalize_server_argv(&once).unwrap(), once);
}

/// popt's refusals, worded and attributed as 3.5.1 prints them.
#[test]
fn refusals_follow_upstream_text_and_side() {
    let refuse = |tokens: &[&str]| normalize_server_argv(&argv(tokens)).unwrap_err();
    assert_eq!(
        refuse(&["rsync", "-vj", "--server", ".", "d/"]),
        ArgvRefusal {
            text: "-vj: unknown option".to_owned(),
            after_server: false,
        }
    );
    assert_eq!(
        refuse(&["rsync", "--server", "-j", ".", "d/"]),
        ArgvRefusal {
            text: "on remote machine: -j: unknown option".to_owned(),
            after_server: true,
        }
    );
    assert_eq!(
        refuse(&["rsync", "-Mfoo", "--server", ".", "d/"]),
        ArgvRefusal {
            text: "Remote option must start with a dash: foo".to_owned(),
            after_server: false,
        }
    );
}

/// Tokens after `--` are operands, whatever they look like.
#[test]
fn operands_after_the_marker_are_untouched() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--", ".", "-j"]),
        ["rsync", "--server", "-r", "--", ".", "-j"]
    );
}

/// The table rows of one kind, each with its letter where it has one.
fn rows(wanted: fn(LongOption) -> Option<char>) -> Vec<(&'static str, char)> {
    LONG_OPTIONS
        .iter()
        .filter_map(|&(name, kind)| wanted(kind).map(|letter| (name, letter)))
        .collect()
}

/// A long name of a short switch is that switch: popt sets the same
/// variable, so the server must read `-M--perms` exactly as `-M-p`.
#[test]
fn every_long_name_of_a_switch_joins_the_flag_string() {
    let letters = rows(|kind| match kind {
        LongOption::Letter(letter) => Some(letter),
        _ => None,
    });
    assert_eq!(letters.len(), 40);
    for (name, letter) in letters {
        let option = format!("--{name}");
        assert_eq!(
            normalized(&["rsync", "--server", "-r", &option, ".", "d/"]),
            ["rsync", "--server", &format!("-r{letter}"), ".", "d/"],
            "{option}"
        );
    }
}

/// A `--no-` negation resets its variable, so every earlier occurrence of
/// the letter stops counting while later ones still do.
#[test]
fn every_negation_clears_its_letter() {
    let negations = rows(|kind| match kind {
        LongOption::Clears(letter) => Some(letter),
        _ => None,
    });
    assert_eq!(negations.len(), 51);
    for (name, letter) in negations {
        let option = format!("--{name}");
        let cluster = format!("-{letter}r{letter}");
        let later = format!("-{letter}");
        assert_eq!(
            normalized(&["rsync", "--server", &cluster, &option, &later, ".", "d/"]),
            ["rsync", "--server", &format!("-r{letter}"), ".", "d/"],
            "{option}"
        );
    }
}

/// upstream: options.c:1686-1702 - `-a` sets each of its variables, so a
/// negation of one leaves the others set.
#[test]
fn a_negation_after_archive_keeps_the_rest_of_it() {
    assert_eq!(
        normalized(&["rsync", "--server", "-av", "--no-p", ".", "d/"]),
        ["rsync", "--server", "-rltgoDv", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-av", "--no-v", ".", "d/"]),
        ["rsync", "--server", "-a", ".", "d/"]
    );
}

/// popt binds a value-taking option's value from the next token; the rewrite
/// joins the two so no value is read as an operand.
#[test]
fn every_split_value_option_is_joined() {
    let values = rows(|kind| (kind == LongOption::Value).then_some('='));
    assert_eq!(values.len(), 58);
    for (name, _) in values {
        let option = format!("--{name}");
        if FILTER_OPTIONS.contains(&option.as_str())
            || is_two_arg_server_long_flag(&option)
            || name == "remote-option"
            || matches!(name, "chown" | "usermap" | "groupmap")
        {
            continue;
        }
        assert_eq!(
            normalized(&["rsync", "--server", "-r", &option, "V", ".", "d/"]),
            ["rsync", "--server", "-r", &format!("{option}=V"), ".", "d/"],
            "{option}"
        );
    }
}

/// `--block-size` and `--rsh` are the `-B` and `-e` entries' long names.
#[test]
fn value_letters_are_read_as_their_short_options() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--block-size=2048", ".", "d/"]),
        ["rsync", "--server", "-r", "-B2048", ".", "d/"]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-re.iLs", "--rsh", "x", ".", "d/"]),
        ["rsync", "--server", "-rex", ".", "d/"]
    );
}

/// upstream: options.c:1961-2000 - `--chown` is `--usermap` / `--groupmap`
/// of `*` plus `-o` / `-g` for each non-empty half.
#[test]
fn chown_becomes_the_maps_it_implies() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--chown=u:g", ".", "d/"]),
        [
            "rsync",
            "--server",
            "-rog",
            "--usermap=*:u",
            "--groupmap=*:g",
            ".",
            "d/"
        ]
    );
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--chown", ":g", ".", "d/"]),
        ["rsync", "--server", "-rg", "--groupmap=*:g", ".", "d/"]
    );
}

/// A switch with a joined value stays for the long-option pass to refuse,
/// and a value-taking option with no value left is popt's refusal.
#[test]
fn malformed_long_options_follow_popt() {
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--perms=1", ".", "d/"]),
        ["rsync", "--server", "-r", "--perms=1", ".", "d/"]
    );
    assert_eq!(
        normalize_server_argv(&argv(&["rsync", "--server", "-r", "--log-file"])).unwrap_err(),
        ArgvRefusal {
            text: "on remote machine: --log-file: missing argument".to_owned(),
            after_server: true,
        }
    );
}

/// The rewrite stays idempotent with long options in it, as the secluded-args
/// list needs.
#[test]
fn long_option_rewrite_is_idempotent() {
    let once = normalized(&[
        "rsync",
        "--server",
        "-av",
        "--no-p",
        "--perms",
        "--block-size",
        "1024",
        "--chown=u:g",
        "--log-file-format",
        "%o",
        ".",
        "d/",
    ]);
    assert_eq!(
        once,
        [
            "rsync",
            "--server",
            "-rltgoDvp",
            "-B1024",
            "--usermap=*:u",
            "--groupmap=*:g",
            "--log-file-format=%o",
            ".",
            "d/"
        ]
    );
    let once: Vec<OsString> = once.into_iter().map(OsString::from).collect();
    assert_eq!(normalize_server_argv(&once).unwrap(), once);
}

/// upstream: options.c:1929-2000 - a second user or group map is refused,
/// with the wording naming which option set the first.
#[test]
fn id_map_conflicts_follow_upstream() {
    let refuse = |tokens: &[&str]| {
        let mut argv = vec!["rsync", "--server", "-r"];
        argv.extend_from_slice(tokens);
        argv.extend_from_slice(&[".", "d/"]);
        normalize_server_argv(&self::argv(&argv)).unwrap_err().text
    };
    assert_eq!(
        refuse(&["--usermap=a:b", "--chown=u"]),
        "--chown conflicts with prior --usermap."
    );
    assert_eq!(
        refuse(&["--groupmap=a:b", "--chown=:g"]),
        "--chown conflicts with prior --groupmap."
    );
    assert_eq!(
        refuse(&["--chown=u", "--usermap=a:b"]),
        "--usermap conflicts with prior --chown."
    );
    assert_eq!(
        refuse(&["--chown=:g", "--groupmap=a:b"]),
        "--groupmap conflicts with prior --chown."
    );
    assert_eq!(
        refuse(&["--chown=u", "--chown=v"]),
        "You can only specify a user-affecting --chown once."
    );
    assert_eq!(
        refuse(&["--chown=:g", "--chown=:h"]),
        "You can only specify a group-affecting --chown once."
    );
    assert_eq!(
        refuse(&["--usermap=a:b", "--usermap=c:d"]),
        "You can only specify --usermap once."
    );
    assert_eq!(
        refuse(&["--groupmap=a:b", "--groupmap=c:d"]),
        "You can only specify --groupmap once."
    );
    // A user part and a group part of separate --chown options do not meet.
    assert_eq!(
        normalized(&[
            "rsync",
            "--server",
            "-r",
            "--chown=u",
            "--chown=:g",
            ".",
            "d/"
        ]),
        [
            "rsync",
            "--server",
            "-rog",
            "--usermap=*:u",
            "--groupmap=*:g",
            ".",
            "d/"
        ]
    );
    // A forwarded map sets the owner switch it implies.
    assert_eq!(
        normalized(&["rsync", "--server", "-r", "--usermap=a:b", ".", "d/"]),
        ["rsync", "--server", "-ro", "--usermap=a:b", ".", "d/"]
    );
}

/// Entries of long_options[] the table leaves out; see the module docs.
const OMITTED: [&str; 10] = [
    "help",
    "version",
    "read-batch",
    "write-batch",
    "copy-as",
    "config",
    "daemon",
    "dparam",
    "detach",
    "no-detach",
];

/// Rows whose kind the generic rule below does not give: switches the
/// long-option pass already reads, and the `OPT_NO_D` / `OPT_NO_COMPRESS`
/// negations, which clear a letter through a `case` rather than a variable
/// (options.c:1708-1710, :1786-1789). `--temp-dir`, `--modify-window`,
/// `--filter` and `--remote-option` keep the long spellings the rewrite
/// already reads.
const SPECIAL: [(&str, LongOption); 16] = [
    ("relative", LongOption::Flag),
    ("no-relative", LongOption::Flag),
    ("no-R", LongOption::Flag),
    ("from0", LongOption::Flag),
    ("no-from0", LongOption::Flag),
    ("no-r", LongOption::Flag),
    ("no-recursive", LongOption::Flag),
    ("no-W", LongOption::Flag),
    ("no-whole-file", LongOption::Flag),
    ("no-D", LongOption::Clears('D')),
    ("no-compress", LongOption::Clears('z')),
    ("no-z", LongOption::Clears('z')),
    ("temp-dir", LongOption::Value),
    ("modify-window", LongOption::Value),
    ("filter", LongOption::Value),
    ("remote-option", LongOption::Value),
];

/// Variables whose short entry is dispatched through `case` (its `argPtr` is
/// 0), so their `--no-` entries name the variable but not the letter.
const CASE_LETTERS: [(&str, char); 9] = [
    ("verbose", 'v'),
    ("preserve_acls", 'A'),
    ("preserve_xattrs", 'X'),
    ("preserve_atimes", 'U'),
    ("preserve_hard_links", 'H'),
    ("human_readable", 'h'),
    ("one_file_system", 'x'),
    ("fuzzy_basis", 'y'),
    ("itemize_changes", 'i'),
];

/// One named long_options[] entry: name, short letter, argInfo, argPtr
/// variable (empty when 0) and val.
struct Entry {
    name: String,
    short: Option<char>,
    arg_info: String,
    var: String,
    val: String,
}

/// The named entries of `long_options[]` in options.c, in order.
fn parse_long_options(source: &str) -> Vec<Entry> {
    let start = source
        .find("static struct poptOption long_options[]")
        .expect("long_options[]");
    let table = &source[start..];
    let table = &table[..table.find("};").expect("table end")];
    table
        .lines()
        .filter_map(|line| {
            let body = line.trim().strip_prefix("{\"")?;
            let (name, rest) = body.split_once('"')?;
            let fields: Vec<&str> = rest
                .trim_start_matches(',')
                .split(',')
                .map(str::trim)
                .collect();
            let short = fields[0].strip_prefix('\'').and_then(|c| c.chars().next());
            let var = fields[2].strip_prefix('&').unwrap_or_default().to_owned();
            Some(Entry {
                name: name.to_owned(),
                short,
                arg_info: fields[1].to_owned(),
                var,
                val: fields[3].to_owned(),
            })
        })
        .collect()
}

/// Derives each row's kind from its entry the way the table was built:
/// value-taking entries are `Value` (or `ValueLetter` with a short name), a
/// short name is `Letter`, a `POPT_ARG_VAL` of 0 on a letter's variable is
/// `Clears`, a `POPT_ARG_VAL` equal to a letter entry's is that `Letter`,
/// anything else is `Flag`.
fn derive_rows(entries: &[Entry]) -> Vec<(String, LongOption)> {
    let letter_of = |var: &str| {
        entries.iter().find_map(|entry| match entry.short {
            Some(letter) if !entry.var.is_empty() && entry.var == var => {
                Some((letter, entry.val.as_str()))
            }
            _ => None,
        })
    };
    entries
        .iter()
        .filter(|entry| !OMITTED.contains(&entry.name.as_str()))
        .map(|entry| {
            let special = SPECIAL.iter().find(|(name, _)| *name == entry.name);
            let case_letter = CASE_LETTERS.iter().find(|(var, _)| *var == entry.var);
            let is_val = entry.arg_info == "POPT_ARG_VAL";
            let kind = match (special, entry.short) {
                (Some(&(_, kind)), _) => kind,
                _ if matches!(entry.arg_info.as_str(), "POPT_ARG_STRING" | "POPT_ARG_INT") => entry
                    .short
                    .map_or(LongOption::Value, LongOption::ValueLetter),
                (None, Some(letter)) => LongOption::Letter(letter),
                _ if is_val && entry.val == "0" => match (case_letter, letter_of(&entry.var)) {
                    (Some(&(_, letter)), _) | (None, Some((letter, _))) => {
                        LongOption::Clears(letter)
                    }
                    _ => LongOption::Flag,
                },
                _ if is_val => match letter_of(&entry.var) {
                    Some((letter, val)) if val == entry.val => LongOption::Letter(letter),
                    _ => LongOption::Flag,
                },
                _ => LongOption::Flag,
            };
            (entry.name.clone(), kind)
        })
        .collect()
}

/// The table is re-derived from rsync 3.5.2's options.c and must equal it
/// row for row. On a difference the derived rows are printed in the table's
/// own syntax, ready to paste.
#[test]
fn table_is_derived_from_upstream_options_c() {
    let Some(path) = test_support::workspace_root()
        .map(|root| root.join("target/interop/upstream-src/rsync-3.5.2/options.c"))
        .filter(|path| path.is_file())
    else {
        test_support::capability_missing(
            "rsync 3.5.2 source not at target/interop/upstream-src/rsync-3.5.2 \
             (fetch it with `tools/ci/run_interop.sh build-only`)",
        );
        return;
    };
    let source = std::fs::read_to_string(&path).expect("read options.c");
    let entries = parse_long_options(&source);
    for name in OMITTED {
        assert!(
            entries.iter().any(|entry| entry.name == name),
            "omitted {name} is not in options.c"
        );
    }
    let derived = derive_rows(&entries);
    let table: Vec<(String, LongOption)> = LONG_OPTIONS
        .iter()
        .map(|&(name, kind)| (name.to_owned(), kind))
        .collect();
    if derived != table {
        let rows: String = derived
            .iter()
            .map(|(name, kind)| format!("    (\"{name}\", LongOption::{kind:?}),\n"))
            .collect();
        panic!(
            "LONG_OPTIONS differs from {}; derived rows:\n{rows}",
            path.display()
        );
    }
}
