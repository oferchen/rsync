//! An anchored module exclude guards its path in both directions.
//!
//! upstream: clientserver.c:937-956 parses a module's `exclude` and `filter`
//! directives with `XFLG_ABS_IF_SLASH`, so `/a1` and `/d/a1` become
//! absolute-path rules. rule_matches() tests such a rule against
//! `curr_dir + module_dirlen + 1` joined to the name (exclude.c:1022-1027),
//! which anchors it at the module root.
//!
//! - Push: the receiving daemon refuses the name with
//!   `ERROR: daemon refused to receive file "<name>"` as FERROR_XFER
//!   (generator.c:1699-1712), keeps the module's copy, and the run exits 23.
//! - Pull: an operand the module hides fails the sender's `is_excluded()`
//!   check with `errno = ENOENT` (flist.c:2948-2957, exclude.c:1111-1115), so
//!   the client sees `link_stat "<name>" (in <module>) failed` and exits 23.
//!
//! Every cell runs the same transfer against a rsync 3.5.2 daemon and an oc
//! daemon, over TCP and over a remote shell, and compares the exit code, the
//! client's stderr and the final tree. The unanchored `exclude = a1` is the
//! control. Without the oracle the cells drive oc's own client and check the
//! expected outcome alone.

#![cfg(unix)]

use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{
    Deadlined, UpstreamVersion, bind_test_listener, require_upstream_rsync, run_deadlined,
};

const BUDGET: Duration = Duration::from_secs(30);
const MODULE: &str = "m";
const NAMES: [&str; 3] = ["a1", "d/a1", "keep"];

/// A module rule and the names it hides, relative to the module root.
struct Rule {
    directive: &'static str,
    hides: &'static [&'static str],
}

const RULES: [Rule; 4] = [
    Rule {
        directive: "exclude = /a1",
        hides: &["a1"],
    },
    Rule {
        directive: "filter = - /a1",
        hides: &["a1"],
    },
    Rule {
        directive: "exclude = /d/a1",
        hides: &["d/a1"],
    },
    Rule {
        directive: "exclude = a1",
        hides: &["a1", "d/a1"],
    },
];

#[derive(Clone, Copy, Debug)]
enum Transport {
    Tcp,
    RemoteShell,
}

const TRANSPORTS: [Transport; 2] = [Transport::Tcp, Transport::RemoteShell];

#[derive(Clone, Copy, Debug)]
enum Op {
    Push,
    /// Push into `mod/blink/`, an in-module symlink to `d/`.
    PushThroughAlias,
    /// Push into `mod/out/`, an in-module symlink to a directory outside
    /// the module.
    PushThroughEscape,
    PullAll,
    PullOperand(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    status: Option<i32>,
    stderr: Vec<String>,
    /// `name=content` for each of [`NAMES`] in the module (push) or in the
    /// destination (pull), `-` when absent.
    tree: Vec<String>,
}

fn oc_rsync() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn seed(dir: &Path, content: &str) {
    for name in NAMES {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, format!("{content}\n")).unwrap();
    }
}

fn tree(dir: &Path) -> Vec<String> {
    NAMES
        .iter()
        .map(|name| {
            let content = fs::read_to_string(dir.join(name)).unwrap_or_else(|_| "-".into());
            format!("{name}={}", content.trim_end())
        })
        .collect()
}

/// The client's stderr with the run's tree replaced and the socket teardown
/// line dropped. After the code-23 trailer a 3.5.2 client may report
/// `[Receiver] read error` or `write error` depending on whether the daemon
/// has already closed, which varies between runs.
fn normalise(stderr: &[u8], root: &Path) -> Vec<String> {
    let root = root.display().to_string();
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|line| {
            !line.starts_with("rsync: [Receiver] read error:")
                && !line.starts_with("rsync: [Receiver] write error:")
        })
        .map(|line| line.replace(&root, "<tree>"))
        .filter(|line| !line.is_empty())
        .collect()
}

/// Runs `op` with `client` against `daemon` serving a module with `directive`.
fn transfer(
    client: &Path,
    daemon: &Path,
    directive: &str,
    transport: Transport,
    op: Op,
) -> Outcome {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let module = root.join("mod");
    let src = root.join("src");
    let dst = root.join("dst");
    seed(&module, "OLD");
    match op {
        Op::Push => seed(&src, "NEWER"),
        Op::PushThroughAlias => {
            seed(&src, "NEWER");
            std::os::unix::fs::symlink("d/", module.join("blink")).unwrap();
        }
        Op::PushThroughEscape => {
            seed(&src, "NEWER");
            seed(&root.join("outside"), "OLD");
            std::os::unix::fs::symlink(root.join("outside"), module.join("out")).unwrap();
        }
        Op::PullAll | Op::PullOperand(_) => fs::create_dir_all(&dst).unwrap(),
    }
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\npid file = {pid}\nlock file = {lock}\n[{MODULE}]\n    \
             path = {module}\n    read only = false\n    {directive}\n",
            pid = root.join("rsyncd.pid").display(),
            lock = root.join("rsyncd.lock").display(),
            module = module.display(),
        ),
    )
    .unwrap();

    let path = match op {
        Op::Push | Op::PullAll => String::new(),
        Op::PushThroughAlias => "blink/".to_owned(),
        Op::PushThroughEscape => "out/".to_owned(),
        Op::PullOperand(name) => name.to_owned(),
    };
    let mut command = Command::new(client);
    command.arg("-r");
    let mut server = None;
    let remote = match transport {
        Transport::Tcp => {
            let listener = bind_test_listener(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
                .expect("bind daemon");
            let inetd = InetdCommand::discarding_stderr(daemon)
                .expect("open /dev/null")
                .arg("--config")
                .arg(&config);
            let process = InetdServer::start(listener, inetd).expect("spawn daemon");
            let url = format!("rsync://127.0.0.1:{}/{MODULE}/{path}", process.port());
            server = Some(process);
            url
        }
        Transport::RemoteShell => {
            let stub = root.join("rsh");
            fs::write(&stub, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").unwrap();
            fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
            command
                .arg(format!("--rsh={}", stub.display()))
                .arg(format!(
                    "--rsync-path={} --config={}",
                    daemon.display(),
                    config.display()
                ));
            format!("host::{MODULE}/{path}")
        }
    };
    match op {
        Op::Push | Op::PushThroughAlias | Op::PushThroughEscape => {
            command.arg(format!("{}/", src.display())).arg(remote)
        }
        Op::PullAll | Op::PullOperand(_) => command.arg(remote).arg(format!("{}/", dst.display())),
    };
    let (status, stderr) = match run_deadlined(&mut command, BUDGET).unwrap() {
        Deadlined::Finished { status, stderr, .. } => (status.code(), stderr),
        Deadlined::Expired { budget, stderr, .. } => panic!(
            "TIMEOUT: {op:?} {directive} over {transport:?} against {} after {budget:?}: {}",
            daemon.display(),
            String::from_utf8_lossy(&stderr)
        ),
    };
    if let Some(process) = server {
        process.finish().expect("finish daemon");
    }
    Outcome {
        status,
        stderr: normalise(&stderr, &root),
        tree: tree(&match op {
            Op::PushThroughEscape => root.join("outside"),
            Op::Push | Op::PushThroughAlias => module,
            Op::PullAll | Op::PullOperand(_) => dst,
        }),
    }
}

/// `strerror(ENOENT)` as this platform's C library spells it.
fn enoent_text() -> String {
    let text = io::Error::from_raw_os_error(2).to_string();
    text.split(" (os error").next().unwrap_or(&text).to_owned()
}

/// Runs `op` for every rule and transport, against the oracle when present,
/// and checks the outcome with `expect`.
fn each_cell(op: Op, expect: impl Fn(&Rule, &Outcome)) {
    let oracle = require_upstream_rsync(UpstreamVersion::V3_5_2).map(|u| u.binary().to_path_buf());
    let client = oracle.clone().unwrap_or_else(oc_rsync);
    for rule in &RULES {
        for transport in TRANSPORTS {
            let actual = transfer(&client, &oc_rsync(), rule.directive, transport, op);
            if let Some(upstream) = &oracle {
                let expected = transfer(&client, upstream, rule.directive, transport, op);
                assert_eq!(
                    actual, expected,
                    "oc daemon vs rsync 3.5.2 daemon: {op:?} with `{}` over {transport:?}",
                    rule.directive
                );
            }
            expect(rule, &actual);
        }
    }
}

/// A push may not overwrite what the module hides: each hidden name is
/// refused out loud, keeps the module's copy, and the run exits 23.
#[test]
fn push_of_a_hidden_name_is_refused() {
    each_cell(Op::Push, |rule, outcome| {
        let context = format!("push with `{}`: {outcome:#?}", rule.directive);
        assert_eq!(outcome.status, Some(23), "{context}");
        for name in NAMES {
            let refusal = format!(r#"ERROR: daemon refused to receive file "{name}""#);
            let hidden = rule.hides.contains(&name);
            assert_eq!(outcome.stderr.contains(&refusal), hidden, "{context}");
            let kept = if hidden { "OLD" } else { "NEWER" };
            assert!(
                outcome.tree.contains(&format!("{name}={kept}")),
                "{context}"
            );
        }
    });
}

/// Naming a hidden file as a pull operand fails like a missing one; naming a
/// visible one transfers it.
#[test]
fn pull_of_a_hidden_operand_fails_like_a_missing_file() {
    for operand in ["a1", "d/a1"] {
        each_cell(Op::PullOperand(operand), |rule, outcome| {
            let context = format!("pull of {operand} with `{}`: {outcome:#?}", rule.directive);
            // upstream: flist.c:2956 reports full_fname(), which renders the
            // operand's directory from the module root (util1.c:1382).
            let shown = if operand.contains('/') {
                format!("/{operand}")
            } else {
                operand.to_owned()
            };
            let link_stat = format!(
                r#"rsync: [sender] link_stat "{shown}" (in {MODULE}) failed: {} (2)"#,
                enoent_text()
            );
            let hidden = rule.hides.contains(&operand);
            assert_eq!(outcome.stderr.contains(&link_stat), hidden, "{context}");
            assert_eq!(
                outcome.status,
                Some(if hidden { 23 } else { 0 }),
                "{context}"
            );
            let basename = operand.rsplit('/').next().unwrap();
            let pulled = format!("{basename}={}", if hidden { "-" } else { "OLD" });
            assert!(outcome.tree.contains(&pulled), "{context}");
        });
    }
}

/// A whole-module pull leaves the hidden names out and is a clean exit 0.
#[test]
fn pull_of_the_module_omits_hidden_names_silently() {
    each_cell(Op::PullAll, |rule, outcome| {
        let context = format!("module pull with `{}`: {outcome:#?}", rule.directive);
        assert_eq!(outcome.status, Some(0), "{context}");
        assert!(outcome.stderr.is_empty(), "{context}");
        for name in NAMES {
            let content = if rule.hides.contains(&name) {
                "-"
            } else {
                "OLD"
            };
            assert!(
                outcome.tree.contains(&format!("{name}={content}")),
                "{context}"
            );
        }
    });
}

/// A push into an in-module symlink is judged by the alias path, not the
/// target. upstream: util1.c change_dir() appends the spelled destination to
/// `curr_dir`, so pushing to `mod/blink/` (`blink -> d/`) names `blink/a1`,
/// which `/d/a1` does not match, and rsync 3.5.2 writes `d/a1` through the
/// link. The unanchored control `a1` still refuses it by basename.
#[test]
fn push_through_an_in_module_symlink_is_judged_by_the_alias_name() {
    each_cell(Op::PushThroughAlias, |rule, outcome| {
        let context = format!("alias push with `{}`: {outcome:#?}", rule.directive);
        let unanchored = !rule.directive.contains('/');
        assert_eq!(
            outcome.status,
            Some(if unanchored { 23 } else { 0 }),
            "{context}"
        );
        let landed = if unanchored { "OLD" } else { "NEWER" };
        assert!(
            outcome.tree.contains(&format!("d/a1={landed}")),
            "{context}"
        );
        assert!(outcome.tree.contains(&"a1=OLD".to_owned()), "{context}");
        assert!(outcome.tree.contains(&"keep=OLD".to_owned()), "{context}");
    });
}

/// An in-module symlink that leaves the module is never written through,
/// whatever the module's rules say. This pins the confinement the daemon had
/// before the alias change: the push fails and the outside tree keeps its
/// content. rsync 3.5.2 also refuses (change_dir#1 ... Too many levels of
/// symbolic links, exit 3); oc's exit code and text for this refusal differ
/// and are tracked separately, so only the outcome is compared here.
#[test]
fn push_through_a_symlink_leaving_the_module_is_refused() {
    let client = require_upstream_rsync(UpstreamVersion::V3_5_2)
        .map(|u| u.binary().to_path_buf())
        .unwrap_or_else(oc_rsync);
    for rule in &RULES {
        for transport in TRANSPORTS {
            let outcome = transfer(
                &client,
                &oc_rsync(),
                rule.directive,
                transport,
                Op::PushThroughEscape,
            );
            let context = format!(
                "escape push with `{}` over {transport:?}: {outcome:#?}",
                rule.directive
            );
            assert_ne!(outcome.status, Some(0), "{context}");
            for name in NAMES {
                assert!(outcome.tree.contains(&format!("{name}=OLD")), "{context}");
            }
        }
    }
}
