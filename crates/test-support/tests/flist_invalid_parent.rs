//! Without INC_RECURSE a receiver aborts on a file-list entry whose parent
//! directory the sender did not list.
//!
//! upstream: generator.c:1730-1752 recv_generator() - "ABORTING due to
//! invalid path from sender: %s/%s" and exit_cleanup(RERR_PROTOCOL). A
//! well-formed sender never produces such a list, so the `rewrite-rsh`
//! relay turns `d/Mxyz` into `dM/xyz` on the wire (same length, so every
//! frame stays valid). `dM` is never listed.
//!
//! Before the check an oc receiver kept going, created what it could and
//! ended on a mkstemp failure (exit 23). rsync 3.5.2 prints the line above,
//! then `rsync error: protocol incompatibility (code 2) at generator.c(1752)
//! [generator=3.5.2]`, and exits 2. oc stops before generating anything, so
//! the destination is left exactly as it was (upstream may already have run
//! `--delete-before` and created earlier directories - a deliberate
//! difference, pinned by `the_destination_is_untouched_by_an_aborted_push`).
//!
//! The control cells show the check refuses nothing a real sender sends.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(feature = "daemon")]
use test_support::oc_daemon::OcDaemon;
use test_support::{
    Deadlined, UpstreamVersion, oc_rsync_bin, require_upstream_rsync, run_deadlined,
};

const BUDGET: Duration = Duration::from_secs(120);
const ABORT_LINE: &str = "ABORTING due to invalid path from sender: dM/xyz";
/// The trailer rsync 3.5.2 prints, with its program name, source location
/// and version cut out (see [`normalise`]).
const ABORT_TRAILER: &str = "error: protocol incompatibility (code 2) at LOC [generator=VER]";

/// A remote shell that runs the server locally, unchanged.
const DIRECT_SHELL: &str = "#!/bin/sh\nshift\nexec \"$@\"\n";

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    src: PathBuf,
    direct: PathBuf,
}

/// `d/Mxyz` is the only entry under `d`, so rewriting it changes no other
/// name (names are sent relative to the previous one).
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().canonicalize().expect("canonical tempdir");
    let src = root.join("src");
    for dir in ["d", "n/e", "zz"] {
        fs::create_dir_all(src.join(dir)).expect("create source dir");
    }
    for (name, body) in [("a1", "a"), ("d/Mxyz", "m"), ("n/e/f", "f"), ("zz/q", "q")] {
        fs::write(src.join(name), body).expect("write source file");
    }
    let direct = root.join("rsh");
    fs::write(&direct, DIRECT_SHELL).expect("write remote shell");
    fs::set_permissions(&direct, fs::Permissions::from_mode(0o755)).expect("chmod remote shell");
    Fixture {
        _temp: temp,
        root,
        src,
        direct,
    }
}

/// Where the rewrite happens: the sender's list travels server->client on a
/// pull and client->server on a push.
#[derive(Clone, Copy)]
enum Rewrite {
    None,
    PullList,
    PushList,
    /// `d/Mxyz` arrives as `d/M/yz` on a pull: `d/M` is missing but `d` is
    /// listed.
    PullBelowListed,
}

struct Outcome {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(fx: &Fixture, client: &Path, server: &Path, rewrite: Rewrite, args: &[String]) -> Outcome {
    let mut command = Command::new(client);
    command
        .current_dir(&fx.root)
        // A fixed seed keeps every byte before the file list deterministic,
        // which the relay's hold-back relies on (see rewrite-rsh).
        .args([
            "-r",
            "--no-inc-recursive",
            "--protocol=31",
            "--checksum-seed=1",
        ])
        .arg(format!("--rsync-path={}", server.display()))
        .arg("-e");
    match rewrite {
        Rewrite::None => {
            command.arg(&fx.direct);
        }
        Rewrite::PullList | Rewrite::PushList | Rewrite::PullBelowListed => {
            command
                .arg(env!("CARGO_BIN_EXE_rewrite-rsh"))
                .env("OC_REWRITE_FROM", "/Mxyz")
                .env(
                    "OC_REWRITE_TO",
                    if matches!(rewrite, Rewrite::PullBelowListed) {
                        "/M/yz"
                    } else {
                        "M/xyz"
                    },
                )
                .env(
                    "OC_REWRITE_DIR",
                    if matches!(rewrite, Rewrite::PushList) {
                        "c2s"
                    } else {
                        "s2c"
                    },
                );
        }
    }
    command.args(args);
    match run_deadlined(&mut command, BUDGET).expect("spawn rsync") {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => Outcome {
            code: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        },
        Deadlined::Expired { .. } => panic!("{args:?}: the transfer stalled"),
    }
}

fn remote(path: &Path) -> String {
    format!("localhost:{}", path.display())
}

/// Sorted relative names under `dir`, without `dir` itself.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("dir entry").path();
            out.push(
                path.strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            );
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The abort report with the program name, source location and version cut
/// out, which are the only parts that may differ from rsync 3.5.2.
fn normalise(stderr: &str) -> Vec<String> {
    stderr
        .lines()
        .filter(|line| line.contains("ABORTING") || line.contains("(code 2)"))
        .map(|line| match line.split_once(" error: ") {
            Some((_, rest)) => {
                let (head, tail) = rest.split_once(" at ").unwrap_or((rest, ""));
                let role = tail.split_once('[').map_or("", |(_, r)| r);
                let role = role.split_once('=').map_or(role, |(r, _)| r);
                format!("error: {head} at LOC [{role}=VER]")
            }
            None => line.to_owned(),
        })
        .collect()
}

fn assert_aborted(what: &str, out: &Outcome) {
    assert_eq!(out.code, 2, "{what}: exit code\n{}", out.stderr);
    assert_eq!(
        normalise(&out.stderr),
        [ABORT_LINE, ABORT_TRAILER],
        "{what}: report\n{}",
        out.stderr
    );
}

/// WHY: the generator must refuse the list before it creates or writes
/// anything, on a pull (client receiver) and a push (server receiver), and
/// report it the way rsync 3.5.2's generator does.
#[test]
fn an_entry_whose_parent_was_not_sent_aborts_the_transfer() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    let src = format!("{}/", fx.src.display());

    let dst = fx.root.join("pull");
    fs::create_dir(&dst).unwrap();
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PullList,
        &[remote(&fx.src) + "/", format!("{}/", dst.display())],
    );
    assert_aborted("pull", &out);
    assert_eq!(
        tree(&dst),
        Vec::<String>::new(),
        "pull: nothing may be written"
    );

    let dst = fx.root.join("push");
    fs::create_dir(&dst).unwrap();
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PushList,
        &[src.clone(), remote(&dst) + "/"],
    );
    assert_aborted("push", &out);
    assert_eq!(
        tree(&dst),
        Vec::<String>::new(),
        "push: nothing may be written"
    );

    // -R sends the implied directory `d`; after the rewrite `dM` is missing.
    let dst = fx.root.join("push-relative");
    fs::create_dir(&dst).unwrap();
    let relative = format!("{}/./d/Mxyz", fx.src.display());
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PushList,
        &["-R".into(), relative, remote(&dst) + "/"],
    );
    assert_aborted("push -R", &out);
    assert_eq!(
        tree(&dst),
        Vec::<String>::new(),
        "push -R: nothing may be written"
    );
}

/// WHY: oc aborts before any generation pass, so an aborted push neither
/// deletes nor creates anything. rsync 3.5.2 aborts from inside its walk,
/// after `--delete-before` and the directories that sort before the bad
/// entry; this pins the earlier stop as a deliberate choice.
#[test]
fn the_destination_is_untouched_by_an_aborted_push() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    let dst = fx.root.join("dst");
    fs::create_dir(&dst).unwrap();
    fs::write(dst.join("extraneous"), "keep").unwrap();
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PushList,
        &[
            "--delete-before".into(),
            format!("{}/", fx.src.display()),
            remote(&dst) + "/",
        ],
    );
    assert_aborted("push --delete-before", &out);
    assert_eq!(tree(&dst), ["extraneous"]);
}

/// WHY: a pulling client checks the list against what it asked for before
/// any generator runs (flist.c:1374 in 3.5.2), so a `-R` pull meets that
/// refusal (exit 4) and never this check.
#[test]
fn a_relative_pull_is_refused_earlier_as_an_unrequested_name() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    let dst = fx.root.join("dst");
    fs::create_dir(&dst).unwrap();
    let relative = remote(&fx.src) + "/./d/Mxyz";
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PullList,
        &["-R".into(), relative, format!("{}/", dst.display())],
    );
    assert_eq!(out.code, 4, "{}", out.stderr);
    assert!(
        out.stderr
            .contains("rejecting unrequested file-list name: dM/xyz"),
        "{}",
        out.stderr
    );
}

/// WHY: rsync 3.5.2 lists a bad list without complaint and does not check
/// it on a dry run into a destination it has not created, or below a
/// directory a dry run found missing (generator.c:1675-1681, :1716, :1883).
#[test]
fn list_only_and_dry_runs_do_not_check() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    let from = remote(&fx.src) + "/";

    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PullList,
        &["--list-only".into(), from.clone()],
    );
    assert_eq!(out.code, 0, "--list-only: {}", out.stderr);
    assert!(out.stdout.contains("dM/xyz"), "--list-only: {}", out.stdout);

    let missing = fx.root.join("missing");
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PullList,
        &["-n".into(), from.clone(), format!("{}/", missing.display())],
    );
    assert_eq!(out.code, 0, "-n into a missing destination: {}", out.stderr);
    assert!(!missing.exists());

    let empty = fx.root.join("empty");
    fs::create_dir(&empty).unwrap();
    let out = run(
        &fx,
        &oc,
        &oc,
        Rewrite::PullList,
        &["-n".into(), from, format!("{}/", empty.display())],
    );
    assert_eq!(out.code, 0, "-n into an empty destination: {}", out.stderr);
    assert_eq!(tree(&empty), Vec::<String>::new());
}

/// WHY: every list a real sender produces names each parent, so the check
/// must leave normal transfers alone: nested trees both ways, `-R` with and
/// without implied directories, a single top-level file and `--files-from`.
#[test]
fn lists_a_real_sender_produces_transfer_unchanged() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    let full = ["a1", "d", "d/Mxyz", "n", "n/e", "n/e/f", "zz", "zz/q"];
    let cells: Vec<(&str, Vec<String>, Vec<&str>, bool)> = vec![
        (
            "nested pull",
            vec![remote(&fx.src) + "/"],
            full.to_vec(),
            true,
        ),
        (
            "nested push",
            vec![format!("{}/", fx.src.display())],
            full.to_vec(),
            false,
        ),
        (
            "-R implied dirs",
            vec!["-R".into(), remote(&fx.src) + "/./n/e/f"],
            vec!["n", "n/e", "n/e/f"],
            true,
        ),
        (
            "-R --no-implied-dirs",
            vec![
                "-R".into(),
                "--no-implied-dirs".into(),
                remote(&fx.src) + "/./n/e/f",
            ],
            vec!["n", "n/e", "n/e/f"],
            true,
        ),
        (
            "single top-level file",
            vec![remote(&fx.src) + "/a1"],
            vec!["a1"],
            true,
        ),
    ];
    for (run_no, (what, from, expect, pull)) in cells.into_iter().enumerate() {
        let dst = fx.root.join(format!("dst{run_no}"));
        fs::create_dir(&dst).unwrap();
        let mut args = from;
        args.push(if pull {
            format!("{}/", dst.display())
        } else {
            remote(&dst) + "/"
        });
        let out = run(&fx, &oc, &oc, Rewrite::None, &args);
        assert_eq!(out.code, 0, "{what}: {}", out.stderr);
        assert_eq!(tree(&dst), expect, "{what}");
    }

    let list = fx.root.join("files-from");
    fs::write(&list, "n/e/f\nzz/q\n").unwrap();
    let dst = fx.root.join("dst-files-from");
    fs::create_dir(&dst).unwrap();
    let args = [
        format!("--files-from={}", list.display()),
        remote(&fx.src) + "/",
        format!("{}/", dst.display()),
    ];
    let out = run(&fx, &oc, &oc, Rewrite::None, &args);
    assert_eq!(out.code, 0, "--files-from: {}", out.stderr);
    assert_eq!(tree(&dst), ["n", "n/e", "n/e/f", "zz", "zz/q"]);
}

/// WHY: below protocol 31 the abort is not announced, so a daemon must close
/// the connection at once or the client waits forever for file requests.
/// Upstream's abort runs in the generator, which waits for the peer
/// (`noop_io_until_death()`) only at protocol 31 and up, because
/// `am_receiver` is clear there (cleanup.c:247-259). A real sender omits the
/// implied directories under `-R --no-implied-dirs` below protocol 30
/// (flist.c:2779-2780, :2994), so no relay is needed. Measured against a
/// rsync 3.5.2 daemon: the client prints the abort and the generator
/// trailer, then `connection unexpectedly closed`, and exits 12.
#[cfg(feature = "daemon")]
#[test]
fn a_daemon_closes_the_connection_after_an_abort_below_protocol_31() {
    let fx = fixture();
    let module = fx.root.join("module");
    fs::create_dir(&module).unwrap();
    let config = fx.root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "log file = {}\nuse chroot = false\n\n[data]\npath = {}\nread only = false\n",
            fx.root.join("rsyncd.log").display(),
            module.display()
        ),
    )
    .unwrap();
    let daemon = OcDaemon::builder(&config).spawn().expect("spawn daemon");
    let mut command = Command::new(oc_rsync_bin());
    command
        .current_dir(&fx.src)
        .args(["-r", "-R", "--no-implied-dirs", "--protocol=29", "./n/e/f"])
        .arg(format!("rsync://127.0.0.1:{}/data/", daemon.port()));
    let Deadlined::Finished { status, stderr, .. } =
        run_deadlined(&mut command, BUDGET).expect("spawn rsync")
    else {
        panic!("the daemon held the connection open after its abort");
    };
    let stderr = String::from_utf8_lossy(&stderr);
    assert_eq!(status.code(), Some(12), "exit code\n{stderr}");
    assert_eq!(
        normalise(&stderr),
        [
            "ABORTING due to invalid path from sender: n/e/f",
            ABORT_TRAILER
        ],
        "report\n{stderr}"
    );
    assert!(
        stderr.contains("connection unexpectedly closed"),
        "the client must see the daemon close\n{stderr}"
    );
    assert!(tree(&module).is_empty(), "module: {:?}", tree(&module));
}

/// WHY: under `--delete-missing-args` a missing operand below a missing
/// directory is a warning, not an abort (generator.c:1745-1746). The sender
/// sends the implied parent `nope` through make_file(), which reports it as
/// vanished (flist.c:2507-2516, :1672-1693) and exits 24; the receiver makes
/// no parent for the mode-0 entry (generator.c:1755, :1786). Measured with
/// rsync 3.5.2 as both ends, pull and push.
#[test]
fn a_missing_arg_below_an_absent_directory_only_warns() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    for (what, pull) in [("pull", true), ("push", false)] {
        let dst = fx.root.join(format!("missing-{what}"));
        fs::create_dir(&dst).unwrap();
        let operands = [
            format!("{}/./n/e/f", fx.src.display()),
            format!("{}/./nope/gone", fx.src.display()),
        ];
        let mut args: Vec<String> = vec!["-R".into(), "--delete-missing-args".into()];
        if pull {
            args.extend(operands.iter().map(|o| format!("localhost:{o}")));
            args.push(format!("{}/", dst.display()));
        } else {
            args.extend(operands);
            args.push(remote(&dst) + "/");
        }
        let out = run(&fx, &oc, &oc, Rewrite::None, &args);
        assert_eq!(out.code, 24, "{what}: exit code\n{}", out.stderr);
        assert!(
            out.stderr
                .contains(&format!("file has vanished: \"{}/nope\"", fx.src.display())),
            "{what}: the sender reports the implied parent\n{}",
            out.stderr
        );
        assert!(
            out.stderr
                .contains("WARNING: parent dir is absent in the file list: nope"),
            "{what}\n{}",
            out.stderr
        );
        assert_eq!(
            tree(&dst),
            ["n", "n/e", "n/e/f"],
            "{what}: no `nope` is made"
        );
    }
}

/// WHY: the sender stats an implied parent with `copy_links` forced on
/// (flist.c:2504), so a dangling symlink there is "symlink has no referent"
/// with IOERR_GENERAL (flist.c:1680-1685, exit 23), not a vanished file.
/// Measured identical with rsync 3.5.2 as both ends.
#[test]
fn a_dangling_symlink_as_implied_parent_has_no_referent() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    std::os::unix::fs::symlink(fx.root.join("nowhere"), fx.src.join("lnk")).unwrap();
    let dst = fx.root.join("dangling");
    fs::create_dir(&dst).unwrap();
    let args = [
        "-R".to_owned(),
        "--delete-missing-args".to_owned(),
        format!("localhost:{}/./n/e/f", fx.src.display()),
        format!("localhost:{}/./lnk/gone", fx.src.display()),
        format!("{}/", dst.display()),
    ];
    let out = run(&fx, &oc, &oc, Rewrite::None, &args);
    assert_eq!(out.code, 23, "{}", out.stderr);
    assert!(
        out.stderr.contains(&format!(
            "symlink has no referent: \"{}/lnk\"",
            fx.src.display()
        )),
        "{}",
        out.stderr
    );
    assert!(!out.stderr.contains("file has vanished"), "{}", out.stderr);
    assert_eq!(tree(&dst), ["n", "n/e", "n/e/f"]);
}

/// Pulls `localhost:src/d` (no trailing slash, so no `.` entry and the
/// destination root is not touched) through the relay that turns `d/Mxyz`
/// into `d/M/yz`.
fn pull_below_listed(
    fx: &Fixture,
    client: &Path,
    server: &Path,
    dst: &Path,
    extra: &[&str],
) -> Outcome {
    let mut args: Vec<String> = extra.iter().map(|a| (*a).to_owned()).collect();
    args.push(remote(&fx.src.join("d")));
    args.push(format!("{}/", dst.display()));
    run(fx, client, server, Rewrite::PullBelowListed, &args)
}

/// The three ways rsync 3.5.2 settles `d/M/yz` below the listed `d`
/// (generator.c:1685-1695 `skip_dir`): `--existing` skips the missing `d`
/// (:1798), a failed mkdir of `d` skips it (:1922), and otherwise the walk
/// reaches the entry, having made `d`, and aborts (:1750-1752).
fn below_listed_cells(
    fx: &Fixture,
    client: &Path,
    server: &Path,
    tag: &str,
) -> Vec<(i32, Vec<String>, Vec<String>)> {
    let mut cells = Vec::new();
    for (cell, extra) in [
        ("control", &[][..]),
        ("existing", &["--existing"][..]),
        ("mkdir", &[][..]),
    ] {
        let dst = fx.root.join(format!("{tag}-{cell}"));
        fs::create_dir(&dst).unwrap();
        if cell == "mkdir" {
            // Without a `.` entry the receiver never raises the root to u+w,
            // so the mkdir of `d` fails. As root it would not, and the
            // assertions below fail rather than pass on the wrong path.
            fs::set_permissions(&dst, fs::Permissions::from_mode(0o555)).unwrap();
        }
        let out = pull_below_listed(fx, client, server, &dst, extra);
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
        let mut lines = normalise(&out.stderr);
        if out
            .stderr
            .contains("*** Skipping any contents from this failed directory ***")
        {
            lines.push("*** Skipping any contents from this failed directory ***".into());
        }
        cells.push((out.code, lines, tree(&dst)));
    }
    cells
}

/// WHY: an entry whose missing parent sits below a listed directory is decided
/// by that directory, as rsync 3.5.2's walk decides it: skipped when the
/// directory is skipped, aborting otherwise.
#[test]
fn an_entry_below_a_skipped_directory_is_skipped() {
    let fx = fixture();
    let oc = oc_rsync_bin();
    let cells = below_listed_cells(&fx, &oc, &oc, "oc");
    assert_eq!(
        cells[0],
        (
            2,
            vec![
                "ABORTING due to invalid path from sender: d/M/yz".to_owned(),
                ABORT_TRAILER.to_owned()
            ],
            vec!["d".to_owned()]
        ),
        "control: `d` is made, then the entry aborts"
    );
    assert_eq!(
        cells[1],
        (0, vec![], vec![]),
        "--existing skips `d` and all below it"
    );
    assert_eq!(
        cells[2],
        (
            23,
            vec!["*** Skipping any contents from this failed directory ***".to_owned()],
            vec![]
        ),
        "the failed mkdir of `d` skips all below it"
    );
}

/// WHY: the three cells above are rsync 3.5.2's own results; run it as the
/// receiver of the same list and require the same exit codes, reports and
/// trees.
#[test]
fn a_skipped_directory_settles_its_entries_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary().to_path_buf();
    let fx = fixture();
    let oc = oc_rsync_bin();
    assert_eq!(
        below_listed_cells(&fx, &oc, &up, "oc"),
        below_listed_cells(&fx, &up, &up, "upstream")
    );
}

/// WHY: the missing-operand cell split by side against rsync 3.5.2: its sender
/// with an oc receiver makes no `nope`, and an oc sender gives its receiver
/// the vanished report and exit 24.
#[test]
fn a_missing_operand_below_a_missing_directory_matches_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary().to_path_buf();
    let fx = fixture();
    let oc = oc_rsync_bin();
    for (what, client, server) in [("3.5.2 sender", &oc, &up), ("oc sender", &up, &oc)] {
        let dst = fx.root.join(what.replace(' ', "-"));
        fs::create_dir(&dst).unwrap();
        let args = [
            "-R".to_owned(),
            "--delete-missing-args".to_owned(),
            format!("localhost:{}/./n/e/f", fx.src.display()),
            format!("localhost:{}/./nope/gone", fx.src.display()),
            format!("{}/", dst.display()),
        ];
        let out = run(&fx, client, server, Rewrite::None, &args);
        assert_eq!(out.code, 24, "{what}\n{}", out.stderr);
        assert!(
            out.stderr
                .contains(&format!("file has vanished: \"{}/nope\"", fx.src.display())),
            "{what}\n{}",
            out.stderr
        );
        assert_eq!(tree(&dst), ["n", "n/e", "n/e/f"], "{what}");
    }
}

/// WHY: the report must be rsync 3.5.2's. When the 3.5.2 binary is
/// available it is run as the receiver of the same crafted list, pull and
/// push, and both reports must normalise to the same two lines.
#[test]
fn the_abort_report_matches_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let up = upstream.binary().to_path_buf();
    let fx = fixture();
    let oc = oc_rsync_bin();
    for (what, client, server, rewrite) in [
        ("pull into 3.5.2", &up, &oc, Rewrite::PullList),
        ("push into 3.5.2", &oc, &up, Rewrite::PushList),
    ] {
        let dst = fx.root.join(what.replace(' ', "-"));
        fs::create_dir(&dst).unwrap();
        let args = if matches!(rewrite, Rewrite::PullList) {
            [remote(&fx.src) + "/", format!("{}/", dst.display())]
        } else {
            [format!("{}/", fx.src.display()), remote(&dst) + "/"]
        };
        let out = run(&fx, client, server, rewrite, &args);
        assert_aborted(what, &out);
    }
}
