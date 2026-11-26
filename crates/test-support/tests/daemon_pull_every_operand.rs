//! A daemon pull naming several sources copies every one of them, over TCP
//! and over a remote shell, and refuses sources that do not belong together
//! the way rsync 3.5.2 does.
//!
//! upstream: main.c:1588-1610 checks each later source against the first
//! (`check_for_hostspec()`, options.c:3379) and keeps its path in
//! `remote_argv[i]`; clientserver.c:354-392 `start_inband_exchange()` then
//! sends every one after `.`. The daemon strips the module name from each
//! (util1.c `glob_expand_module()`).
//!
//! Every cell asserts the destination tree and exit code measured with a
//! 3.5.2 client. When the 3.5.2 oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`,
//! required under `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs it as the
//! client against the same daemon and must give the same tree and exit code.

#![cfg(all(unix, feature = "daemon"))]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;
use test_support::oc_daemon::OcDaemon;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// A module holding `I`, `J`, `K` and `d/f`, served by an oc daemon, plus a
/// remote shell that runs its command locally.
struct Fixture {
    dir: TempDir,
    daemon: OcDaemon,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let module = dir.path().join("mod");
        fs::create_dir_all(module.join("d")).expect("mkdir mod/d");
        fs::create_dir_all(module.join("src/sub")).expect("mkdir mod/src/sub");
        for (name, body) in [
            ("I", "i\n"),
            ("J", "j\n"),
            ("K", "k\n"),
            ("d/f", "f\n"),
            ("src/sub/f", "s\n"),
        ] {
            fs::write(module.join(name), body).expect("write source");
        }
        let config = dir.path().join("rsyncd.conf");
        fs::write(
            &config,
            format!(
                "use chroot = no\n[mod]\npath = {}\nread only = yes\n",
                module.display()
            ),
        )
        .expect("write config");
        // The client runs `<rsh> <host> <command...>`; drop the host and run
        // the command here.
        let rsh = dir.path().join("lsh");
        fs::write(&rsh, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").expect("write rsh");
        fs::set_permissions(&rsh, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
        let daemon = OcDaemon::builder(&config).spawn().expect("spawn daemon");
        Self { dir, daemon }
    }

    fn url(&self, path: &str) -> String {
        format!("rsync://127.0.0.1:{}/mod/{path}", self.daemon.port())
    }

    /// Options that reach the same module through a remote-shell daemon.
    fn rsh_options(&self) -> Vec<String> {
        vec![
            "-e".to_owned(),
            self.dir.path().join("lsh").display().to_string(),
            format!(
                "--rsync-path={} --config={}",
                oc_rsync_bin().display(),
                self.dir.path().join("rsyncd.conf").display()
            ),
        ]
    }

    /// Runs `client` with `args` into a fresh destination and returns its
    /// output and the destination tree.
    fn pull(&self, client: &Path, args: &[String]) -> (Output, Vec<String>) {
        let dest = self.dir.path().join("dst");
        if dest.exists() {
            fs::remove_dir_all(&dest).expect("clear destination");
        }
        fs::create_dir(&dest).expect("mkdir destination");
        let output = Command::new(client)
            .args(args)
            .arg(format!("{}/", dest.display()))
            .output()
            .expect("run client");
        (output, tree(&dest))
    }
}

/// Every entry under `root`, relative, sorted.
fn tree(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            out.push(
                path.strip_prefix(root)
                    .expect("under root")
                    .display()
                    .to_string(),
            );
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    out.sort();
    out
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// One cell: the oc client must give `rc` and `expected`, and so must 3.5.2.
fn check(fixture: &Fixture, cell: &str, args: &[String], rc: i32, expected: &[&str]) {
    let (oc, oc_tree) = fixture.pull(&oc_rsync_bin(), args);
    assert_eq!(oc.status.code(), Some(rc), "{cell}: {oc:?}");
    assert_eq!(oc_tree, expected, "{cell}: {oc:?}");
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let (reference, reference_tree) = fixture.pull(upstream.binary(), args);
        assert_eq!(
            (reference.status.code(), reference_tree),
            (oc.status.code(), oc_tree),
            "{cell}: oc differs from rsync 3.5.2: {reference:?}"
        );
    }
}

#[test]
fn tcp_pull_copies_every_source() {
    let f = Fixture::new();
    let two = [f.url("I"), f.url("J")];
    check(
        &f,
        "two files",
        &[&["-t".to_owned()][..], &two].concat(),
        0,
        &["I", "J"],
    );
    let three = [f.url("I"), f.url("J"), f.url("K")];
    check(
        &f,
        "three files",
        &[&["-t".to_owned()][..], &three].concat(),
        0,
        &["I", "J", "K"],
    );
    let dir_and_file = [f.url("d"), f.url("I")];
    check(
        &f,
        "-r directory and file",
        &[&["-rt".to_owned()][..], &dir_and_file].concat(),
        0,
        &["I", "d", "d/f"],
    );
    let relative = [f.url("I"), f.url("d/f")];
    check(
        &f,
        "-R",
        &[&["-Rt".to_owned()][..], &relative].concat(),
        0,
        &["I", "d", "d/f"],
    );
}

#[test]
fn remote_shell_pull_copies_every_source() {
    let f = Fixture::new();
    let rsh = f.rsh_options();
    for (cell, flag, sources, expected) in [
        (
            "two files",
            "-t",
            &["h::mod/I", "h::mod/J"][..],
            &["I", "J"][..],
        ),
        (
            "three files",
            "-t",
            &["h::mod/I", "h::mod/J", "h::mod/K"],
            &["I", "J", "K"],
        ),
        (
            "-r directory and file",
            "-rt",
            &["h::mod/d", "h::mod/I"],
            &["I", "d", "d/f"],
        ),
        ("-R", "-Rt", &["h::mod/I", "h::mod/d/f"], &["I", "d", "d/f"]),
    ] {
        let args = [strings(&[flag]), rsh.clone(), strings(sources)].concat();
        check(&f, cell, &args, 0, expected);
    }
}

/// A missing first source no longer hides the later ones.
#[test]
fn missing_first_source_still_copies_the_rest() {
    let f = Fixture::new();
    for option in ["--ignore-missing-args", "--delete-missing-args"] {
        let args = strings(&["-t", option])
            .into_iter()
            .chain([f.url("nope"), f.url("J")])
            .collect::<Vec<_>>();
        check(&f, option, &args, 0, &["J"]);
        // The -R directory shape the delete-parity rows found.
        let relative = strings(&["-rtR", option])
            .into_iter()
            .chain([f.url("src/nope/"), f.url("src/sub/")])
            .collect::<Vec<_>>();
        check(
            &f,
            &format!("-R {option}"),
            &relative,
            0,
            &["src", "src/sub", "src/sub/f"],
        );
    }
}

/// Spellings `check_for_hostspec()` treats as the same machine and port.
#[test]
fn equivalent_spellings_are_one_pull() {
    let f = Fixture::new();
    for (cell, second) in [
        ("host::module", "127.0.0.1::mod/J"),
        ("URL without a host", "rsync:///mod/J"),
        ("::module without a host", "::mod/J"),
    ] {
        let args = [strings(&["-t"]), vec![f.url("I")], strings(&[second])].concat();
        check(&f, cell, &args, 0, &["I", "J"]);
    }
}

/// The refusal's text lines, then exactly one code-1 trailer tagged
/// `[Receiver=`, as `rprintf(FERROR, ...)` and `exit_cleanup(RERR_SYNTAX)`
/// print them (main.c:1591-1604, log.c:1025). The trailer names each
/// implementation's own program, source line and version, so only its fixed
/// parts are compared.
fn assert_refusal_shape(cell: &str, who: &str, output: &Output, text: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = stderr.lines().collect();
    let text_lines: Vec<&str> = text.lines().collect();
    assert_eq!(output.status.code(), Some(1), "{cell} ({who}): {stderr:?}");
    assert_eq!(
        lines.len(),
        text_lines.len() + 1,
        "{cell} ({who}): {stderr:?}"
    );
    assert_eq!(lines[..text_lines.len()], text_lines, "{cell} ({who})");
    let trailer = lines[text_lines.len()];
    assert!(
        trailer.contains(" error: syntax or usage error (code 1) at ")
            && trailer.contains(" [Receiver="),
        "{cell} ({who}): {trailer:?}"
    );
}

/// One refusal cell: nothing is copied, and oc and 3.5.2 print the same text
/// in the same shape.
fn check_refusal(f: &Fixture, cell: &str, second: String, text: &str) {
    let args = vec!["-t".to_owned(), f.url("I"), second];
    let (oc, oc_tree) = f.pull(&oc_rsync_bin(), &args);
    assert!(oc_tree.is_empty(), "{cell}: {oc_tree:?}");
    assert_refusal_shape(cell, "oc", &oc, text);
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let (reference, reference_tree) = f.pull(upstream.binary(), &args);
        assert!(reference_tree.is_empty(), "{cell}: {reference_tree:?}");
        assert_refusal_shape(cell, "3.5.2", &reference, text);
    }
}

/// upstream: main.c:1596-1597.
#[test]
fn a_source_on_another_machine_is_refused() {
    let f = Fixture::new();
    let second = format!("rsync://localhost:{}/mod/J", f.daemon.port());
    check_refusal(
        &f,
        "other host",
        second,
        "All source args must come from the same machine.",
    );
}

/// upstream: main.c:1599-1604 - both ports set and different.
#[test]
fn a_source_on_another_port_is_refused() {
    let f = Fixture::new();
    let second = format!(
        "rsync://127.0.0.1:{}/mod/J",
        f.daemon.port().wrapping_add(1)
    );
    check_refusal(
        &f,
        "other port",
        second,
        "All source args must use the same port number.",
    );
}

/// upstream: main.c:1599-1604 - a single-colon source clears the port.
#[test]
fn a_shell_source_after_a_daemon_source_is_refused() {
    let f = Fixture::new();
    check_refusal(
        &f,
        "shell spec",
        "127.0.0.1:/mod/J".to_owned(),
        "All source args must use the same hostspec format.",
    );
}

/// upstream: main.c:1590-1594.
#[test]
fn a_local_source_after_a_daemon_source_is_refused() {
    let f = Fixture::new();
    let local = f.dir.path().join("mod/J").display().to_string();
    let text = format!(
        "Unexpected local arg: {local}\nIf arg is a remote file/dir, prefix it with a colon (:)."
    );
    check_refusal(&f, "local path", local, &text);
}

/// Two modules: the daemon resolves every later source inside the first
/// source's module, so `other/J` is `mod/other/J` there and is missing
/// (clientserver.c:354-392 sends it unchanged; util1.c glob_expand_module()
/// strips only the first source's module name).
#[test]
fn a_source_in_another_module_is_looked_up_in_the_first_module() {
    let f = Fixture::new();
    let second = format!("rsync://127.0.0.1:{}/other/J", f.daemon.port());
    let args = vec!["-t".to_owned(), f.url("I"), second];
    check(&f, "other module", &args, 23, &["I"]);
}

/// The `::` short forms with `--port`: a later `::mod/J` names no host, so it
/// is the first source's machine (main.c:1596); a first source with no host
/// at all has nothing to connect to, and 3.5.2 fails in getaddrinfo with
/// RERR_SOCKETIO (clientserver.c:169).
#[test]
fn double_colon_short_forms_follow_the_first_source() {
    let f = Fixture::new();
    let port = format!("--port={}", f.daemon.port());
    let mixed = strings(&["-t", &port, "127.0.0.1::mod/I", "::mod/J"]);
    check(&f, "host::mod/I ::mod/J", &mixed, 0, &["I", "J"]);
    let bare = strings(&["-t", &port, "::mod/I", "::mod/J"]);
    check(&f, "::mod/I ::mod/J", &bare, 10, &[]);
}
