//! Filter options a client forwards to a daemon with `-M` / `--remote-option`.
//!
//! An upstream client never puts filter rules in the server argv on its own
//! (`server_options()` sends them over the filter list), but `-M` hands any
//! option to the remote `parse_arguments()`, which applies it exactly as on a
//! command line. On a daemon that is a client-chosen file open, so every case
//! here is checked against what upstream 3.5.1 does on the same fixture.
//!
//! upstream: options.c:1620-1656 - `--filter`/`--exclude`/`--include` go to
//! `filter_list` ahead of the rules the client later sends; for
//! `--exclude-from`/`--include-from` the path is sanitized when
//! `sanitize_paths` is set (clientserver.c:1067-1068 sets it for a module
//! without chroot), a name hidden by `daemon_filter_list` rejects the options
//! (options.c:2684), and the file is otherwise opened with
//! `XFLG_FATAL_ERRORS`. A `merge` rule opens through `parse_filter_file`,
//! which treats a daemon-hidden file as absent (exclude.c:1638-1659).
//!
//! MEASURED against rsync 3.5.1 (daemon and client, `use chroot = no`, module
//! `exclude = secret`): every expectation below is upstream's observed result.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
use test_support::{DaemonProcess, oc_rsync_bin};

/// A daemon module whose root holds `secret` (`- decoy`), `rules`
/// (`- keep2`) and the files `decoy`, `keep`, `keep2` and `x`.
struct Fixture {
    temp: TempDir,
    daemon: DaemonProcess,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("tempdir");
        let module = temp.path().join("mod");
        fs::create_dir_all(&module).expect("create module");
        fs::write(module.join("secret"), b"- decoy\n").expect("write secret");
        fs::write(module.join("rules"), b"- keep2\n").expect("write rules");
        for name in ["decoy", "keep", "keep2", "x"] {
            fs::write(module.join(name), name.as_bytes()).expect("write module file");
        }
        let config = temp.path().join("rsyncd.conf");
        fs::write(
            &config,
            format!(
                "log file = {}\n[mod]\n    path = {}\n    use chroot = false\n    read only = false\n    exclude = secret\n",
                temp.path().join("rsyncd.log").display(),
                module.display(),
            ),
        )
        .expect("write rsyncd.conf");
        let daemon =
            DaemonProcess::spawn(&config, std::iter::empty::<&str>()).expect("spawn daemon");
        Self { temp, daemon }
    }

    fn module(&self) -> PathBuf {
        self.temp.path().join("mod")
    }

    fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/mod/", self.daemon.port())
    }

    /// Pulls the module into a fresh directory with `args` before the operands.
    fn pull(&self, args: &[&str]) -> (Output, PathBuf) {
        let dest = self.temp.path().join("dst");
        let _ = fs::remove_dir_all(&dest);
        fs::create_dir_all(&dest).expect("create dest");
        let output = Command::new(oc_rsync_bin())
            .arg("-r")
            .args(args)
            .arg(self.url())
            .arg(format!("{}/", dest.display()))
            .env("OC_RSYNC_FALLBACK", "0")
            .output()
            .expect("run client");
        (output, dest)
    }
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Asserts the client saw the daemon close the session after `@RSYNCD: OK`
/// without a word, the way upstream does when `parse_arguments()` dies.
///
/// upstream: exclude.c:1714-1719 (`exit_cleanup(RERR_FILEIO)`) and
/// exclude.c:136 (`exit_cleanup(RERR_SYNTAX)`) exit from inside the daemon's
/// `parse_arguments()` (clientserver.c:1160), before the `setup_protocol()` +
/// `io_start_multiplex_out()` pair that would carry an error back
/// (clientserver.c:1226-1252). The client hits EOF reading the protocol setup:
/// io.c:299-303 whine_about_eof(), rc 12.
fn assert_silent_daemon_exit(output: &Output, context: &str) {
    let text = stderr(output);
    assert_eq!(output.status.code(), Some(12), "{context}: {text}");
    assert!(
        text.contains("connection unexpectedly closed (0 bytes received so far)"),
        "{context}: {text}"
    );
    assert!(
        !text.contains("@ERROR"),
        "{context}: upstream sends nothing after @RSYNCD: OK: {text}"
    );
}

#[test]
fn exclude_from_a_daemon_hidden_file_rejects_the_options() {
    let fixture = Fixture::new();
    let (output, dest) = fixture.pull(&["-M--exclude-from=secret"]);
    assert!(
        !output.status.success(),
        "upstream refuses the session (rc 10)"
    );
    assert!(
        stderr(&output).contains("Your options have been rejected by the server."),
        "the options.c:2684 message must reach the client: {}",
        stderr(&output)
    );
    assert!(names(&dest).is_empty(), "nothing may be transferred");
}

#[test]
fn absolute_exclude_from_is_confined_to_the_module() {
    let fixture = Fixture::new();
    // A rules file OUTSIDE the module that would exclude `keep` if opened as
    // written, and the module-local file the sanitized path names, which
    // excludes `keep2`. Upstream reads only the module-local one.
    let outside = fixture.temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    let outside_rules = outside.join("rules");
    fs::write(&outside_rules, b"- keep\n").expect("write outside rules");
    let sanitized = outside_rules.strip_prefix("/").expect("absolute temp path");
    let local = fixture.module().join(sanitized);
    fs::create_dir_all(local.parent().expect("parent")).expect("create local parent");
    fs::write(&local, b"- keep2\n").expect("write module-local rules");
    let arg = format!("-M--exclude-from={}", outside_rules.display());
    let (output, dest) = fixture.pull(&[&arg]);
    assert!(
        output.status.success(),
        "rc 0 like upstream: {}",
        stderr(&output)
    );
    let got = names(&dest);
    assert!(
        got.contains(&"keep".to_owned()),
        "the host file was opened: keep was excluded"
    );
    assert!(
        !got.contains(&"keep2".to_owned()),
        "the module-local file the sanitized path names must apply"
    );
}

#[test]
fn absolute_exclude_from_with_no_module_copy_is_fatal() {
    let fixture = Fixture::new();
    let (output, dest) = fixture.pull(&["-M--exclude-from=/etc/passwd"]);
    assert!(
        !output.status.success(),
        "upstream resolves /etc/passwd inside the module, finds nothing and fails"
    );
    assert!(names(&dest).is_empty(), "nothing may be transferred");
}

#[test]
fn filter_merge_of_a_hidden_file_is_treated_as_absent() {
    let fixture = Fixture::new();
    let (output, dest) = fixture.pull(&["-M--filter=merge secret"]);
    assert!(
        output.status.success(),
        "rc 0 like upstream: {}",
        stderr(&output)
    );
    assert!(
        names(&dest).contains(&"decoy".to_owned()),
        "the daemon read `secret`, which `exclude = secret` hides"
    );
}

#[test]
fn filter_merge_of_a_visible_file_is_applied() {
    let fixture = Fixture::new();
    let (output, dest) = fixture.pull(&["-M--filter=merge rules"]);
    assert!(
        output.status.success(),
        "rc 0 like upstream: {}",
        stderr(&output)
    );
    let got = names(&dest);
    assert!(
        !got.contains(&"keep2".to_owned()),
        "the visible merge file must apply"
    );
    assert!(
        got.contains(&"keep".to_owned()),
        "control: the transfer ran"
    );
}

#[test]
fn forwarded_exclude_is_applied() {
    let fixture = Fixture::new();
    let (output, dest) = fixture.pull(&["-M--exclude=x"]);
    assert!(
        output.status.success(),
        "rc 0 like upstream: {}",
        stderr(&output)
    );
    let got = names(&dest);
    assert!(
        !got.contains(&"x".to_owned()),
        "the forwarded exclude must apply"
    );
    assert!(
        got.contains(&"keep".to_owned()),
        "control: the transfer ran"
    );
}

#[test]
fn forwarded_rules_precede_the_client_filter_list() {
    // The daemon sender holds `+ keep` from argv ahead of the client stream
    // `- keep`, so it sends `keep`; the client receiver then refuses it as an
    // excluded file-list name (rc 4). Were the argv rule after the stream
    // rule, `keep` would never be sent and the run would succeed.
    let fixture = Fixture::new();
    let (output, _dest) = fixture.pull(&["-M--include=keep", "--exclude=keep"]);
    assert_eq!(
        output.status.code(),
        Some(4),
        "upstream rc 4: {}",
        stderr(&output)
    );
}

#[test]
fn forwarded_exclude_protects_a_file_from_push_delete() {
    let fixture = Fixture::new();
    let src = fixture.temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("keep"), b"keep").expect("write src keep");
    fs::write(fixture.module().join("zap"), b"zap").expect("write zap");
    let output = Command::new(oc_rsync_bin())
        .args(["-r", "--delete", "-M--exclude=x"])
        .arg(format!("{}/", src.display()))
        .arg(fixture.url())
        .env("OC_RSYNC_FALLBACK", "0")
        .output()
        .expect("run client");
    assert!(
        output.status.success(),
        "rc 0 like upstream: {}",
        stderr(&output)
    );
    let module = fixture.module();
    assert!(
        module.join("x").is_file(),
        "the daemon receiver deleted x despite -M--exclude=x"
    );
    assert!(!module.join("zap").exists(), "control: --delete ran");
}

#[test]
fn exclude_from_through_a_symlink_leaving_the_module_is_refused() {
    // `link` sits in the module but points at a rules file outside it that
    // would exclude `keep`. upstream confines the open to the module root
    // (exclude.c:1686-1690), so the read fails and the session ends.
    let fixture = Fixture::new();
    let outside = fixture.temp.path().join("outside");
    fs::create_dir_all(&outside).expect("create outside");
    fs::write(outside.join("rules"), b"- keep\n").expect("write outside rules");
    std::os::unix::fs::symlink(outside.join("rules"), fixture.module().join("link"))
        .expect("plant symlink");
    let (output, dest) = fixture.pull(&["-M--exclude-from=link"]);
    assert_silent_daemon_exit(&output, "escaping -M--exclude-from symlink");
    assert!(names(&dest).is_empty(), "nothing may be transferred");
}

#[test]
fn fatal_argv_filter_errors_end_the_session_silently() {
    // MEASURED against rsync 3.5.1: each of these exits the daemon inside
    // parse_arguments(), leaving the client rc 12 and the reason only in the
    // daemon log - RERR_FILEIO (11) for an open failure, RERR_SYNTAX (1) for a
    // malformed rule - followed by log_exit()'s totals line (cleanup.c:222-226).
    let fixture = Fixture::new();
    for (arg, cause, code) in [
        (
            "-M--exclude-from=missing",
            "failed to open exclude file missing",
            11,
        ),
        (
            "-M--include-from=missing",
            "failed to open include file missing",
            11,
        ),
        (
            "-M--filter=merge missing",
            "failed to open exclude file missing",
            11,
        ),
        (
            "-M--filter=bogus rule",
            "Unknown filter rule: bogus rule",
            1,
        ),
    ] {
        let (output, dest) = fixture.pull(&[arg]);
        assert_silent_daemon_exit(&output, arg);
        assert!(names(&dest).is_empty(), "{arg}: nothing may be transferred");
        let log = fs::read_to_string(fixture.temp.path().join("rsyncd.log")).expect("read log");
        let tail = &log[log.rfind(cause).unwrap_or_else(|| panic!("{arg}: {log}"))..];
        let (line, rest) = tail.split_once('\n').expect("log line ends");
        assert!(line.contains(&format!("(code {code})")), "{arg}: {line}");
        assert!(
            rest.contains("sent 0 bytes  received 0 bytes  total size 0"),
            "{arg}: {log}"
        );
    }
}
