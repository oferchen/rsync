//! A daemon module's own filter directives, as 3.5.1 applies them.
//!
//! upstream: clientserver.c:929-954 builds `daemon_filter_list` from `filter`,
//! `include from`, `include`, `exclude from` and `exclude` through the same
//! parse_filter_str()/parse_filter_file() as every other rule, with
//! `XFLG_ABS_IF_SLASH | XFLG_DIR2WILD3`. A directive the parser refuses ends
//! the session before `@RSYNCD: OK` with the cause in the daemon log only
//! (exclude.c:133-137, :1714-1719). Every cell pulls the same module from a
//! 3.5.1 daemon and from oc's, with the 3.5.1 client under a deadline, and
//! compares the exit, the served files and the logged cause.

#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::{
    DaemonProcess, Deadlined, UpstreamVersion, require_upstream_rsync, run_deadlined,
};

const BUDGET: Duration = Duration::from_secs(30);

/// The module's files: bait and control names, a directory both at the root
/// and one level down, and a temp file.
const MODULE_FILES: &[&str] = &["a", "bait", "keep", "x.tmp", "d/x", "sub/a", "sub/d/x"];

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn upstream() -> Option<PathBuf> {
    require_upstream_rsync(UpstreamVersion::V3_5_1).map(|u| u.binary().to_path_buf())
}

#[derive(Debug, PartialEq, Eq)]
struct Served {
    status: Option<i32>,
    /// Every file the pull delivered, sorted.
    files: Vec<String>,
    /// The cause the daemon logged for a refused module, if any.
    cause: Option<String>,
}

/// Pulls module `m` from `daemon`, configured with `params` (one directive per
/// line). `{RULES}` in a directive names a file outside the module holding
/// `rules`.
fn module_pull(daemon: &Path, params: &[&str], rules: &str) -> Served {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let module = root.join("mod");
    for name in MODULE_FILES {
        let path = module.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{name}\n")).unwrap();
    }
    fs::create_dir_all(root.join("dst")).unwrap();
    let rules_path = root.join("rules");
    fs::write(&rules_path, rules).unwrap();
    let log = root.join("log");
    let mut config = format!(
        "use chroot = no\nlog file = {}\n[m]\n    path = {}\n",
        log.display(),
        module.display()
    );
    for param in params {
        let param = param.replace("{RULES}", &rules_path.display().to_string());
        config.push_str(&format!("    {param}\n"));
    }
    let config_path = root.join("rsyncd.conf");
    fs::write(&config_path, config).unwrap();
    let process = DaemonProcess::spawn_binary(daemon, &config_path, std::iter::empty::<&str>())
        .expect("spawn daemon");
    let mut command = Command::new(upstream().unwrap());
    command
        .arg("-r")
        .arg(format!("rsync://127.0.0.1:{}/m/", process.port()))
        .arg(format!("{}/", root.join("dst").display()));
    let status = match run_deadlined(&mut command, BUDGET).unwrap() {
        Deadlined::Finished { status, .. } => status.code(),
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "TIMEOUT: module {params:?} on {} still running after {budget:?}: {}",
            daemon.display(),
            String::from_utf8_lossy(&stderr)
        ),
    };
    drop(process);
    Served {
        status,
        files: files_under(&root.join("dst")),
        cause: logged_cause(&fs::read_to_string(&log).unwrap_or_default(), &root),
    }
}

fn files_under(dir: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path.strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    files.sort();
    files
}

/// The refusal's cause from the daemon log, with the run's own tree and each
/// implementation's framing removed: upstream logs the cause bare (an
/// `rsyserr()` one behind `rsync: [Receiver] `) and then an `rsync error:`
/// trailer, oc logs it inside its error line. The log line's framing is the
/// daemon logging's own concern; the cause is what the parser decides.
fn logged_cause(log: &str, root: &Path) -> Option<String> {
    let tree = root.display().to_string();
    log.lines()
        .filter_map(|line| line.split_once("] ").map(|(_, text)| text))
        .find_map(|text| {
            if let Some(rest) = text.strip_prefix("oc-rsync error: ") {
                return rest
                    .rsplit_once(" (code ")
                    .map(|(cause, _)| cause.to_owned());
            }
            let cause = text.contains("filter rule")
                || text.contains("failed to open")
                || text.contains("trailing characters");
            let text = text.strip_prefix("rsync: [Receiver] ").unwrap_or(text);
            (cause && !text.starts_with("rsync error")).then(|| text.to_owned())
        })
        .map(|cause| cause.replace(&tree, "<tree>"))
}

/// Asserts both daemons serve `params` alike, returning what they agree on.
fn assert_parity(params: &[&str], rules: &str) -> Option<Served> {
    let upstream = upstream()?;
    let expected = module_pull(&upstream, params, rules);
    let actual = module_pull(&oc_rsync(), params, rules);
    assert_eq!(
        actual, expected,
        "oc vs upstream 3.5.1 for {params:?} / {rules:?}"
    );
    Some(actual)
}

/// Rows A and B: the `!` (negate) and `/` (absolute-path) modifiers on a
/// module's own rule. The daemon's old parser dropped `!` and anchored `/`,
/// serving files 3.5.1 hides.
#[test]
fn negate_and_abs_path_modifiers_match_upstream() {
    for rule in [
        "filter = -! *.tmp",
        "filter = exclude,! *.tmp",
        "filter = -/ a",
    ] {
        if let Some(served) = assert_parity(&[rule], "") {
            assert_eq!(served.status, Some(0), "{rule}");
        }
    }
}

/// Row C: `-sr` names both sides, so XFLG_ABS_IF_SLASH's add-time drop of a
/// sender-only rule (exclude.c:279-285) keeps it, and `-s` alone drops it.
#[test]
fn a_rule_naming_both_sides_is_kept_and_a_sender_rule_dropped() {
    for rule in ["filter = -sr bait", "filter = -s bait", "filter = -r bait"] {
        assert_parity(&[rule], "");
    }
}

/// XFLG_ABS_IF_SLASH counts the slashes of the pattern without its trailing
/// one (exclude.c:287-306), so `d/` is unanchored and hides `sub/d` too, while
/// `sub/d/` is anchored. XFLG_DIR2WILD3 turns both into `.../***`.
#[test]
fn a_trailing_slash_does_not_anchor_a_module_rule() {
    for rule in ["exclude = d/", "exclude = sub/d/", "filter = - d/"] {
        assert_parity(&[rule], "");
    }
}

/// The `include`/`exclude` directives are word-split with XFLG_OLD_PREFIXES:
/// `- ` and `+ ` prefix one word, and a bare `!` empties the list.
#[test]
fn old_prefix_directives_split_into_words() {
    for rule in [
        "exclude = - bait keep",
        "exclude = bait ! keep",
        "include = + bait - keep",
    ] {
        assert_parity(&[rule, "exclude = *"], "");
    }
}

/// A `!` read from a merge file empties the module's whole list, the rules
/// ahead of the merge included (exclude.c:1542-1551).
#[test]
fn a_clear_inside_a_merge_file_empties_the_module_list() {
    let rules = "!\n- keep\n";
    for rule in ["filter = - bait merge {RULES}", "filter = - bait . {RULES}"] {
        if let Some(served) = assert_parity(&[rule], rules) {
            assert!(
                served.files.iter().any(|f| f == "bait"),
                "{rule}: {served:?}"
            );
        }
    }
}

/// `.e` excludes the merge file's basename before the file is read, and the
/// open is gated on the list built so far (exclude.c:1558-1572, :1639-1664).
/// The gate skips the module root's length from an absolute name whatever it
/// is, so an operator file outside the module is still read.
#[test]
fn a_self_excluding_merge_gates_its_own_open() {
    assert_parity(&["filter = .e {RULES}"], "- bait\n");
    assert_parity(&["filter = dir-merge,e rules"], "");
}

/// A refused directive ends the session before `@RSYNCD: OK` with the cause in
/// the daemon log only: a bad rule (exclude.c:133-137, the rest of the string
/// from the rule on), a bad record in a merge file (reported by where it came
/// from, exclude.c:71-136) and a missing file (exclude.c:1714-1719).
#[test]
fn a_refused_directive_is_logged_and_sends_nothing() {
    let cells: [(&[&str], &str); 4] = [
        (&["filter = - foo clear,- keep"], ""),
        (&["filter = merge {RULES}"], "bait\n"),
        (&["filter = merge,w {RULES}"], "#note\n"),
        (&["exclude from = /nonexistent/excludes"], ""),
    ];
    for (params, rules) in cells {
        if let Some(served) = assert_parity(params, rules) {
            assert_ne!(served.status, Some(0), "{params:?}");
            assert!(served.files.is_empty(), "{params:?}");
            assert!(served.cause.is_some(), "{params:?}: no logged cause");
        }
    }
}
