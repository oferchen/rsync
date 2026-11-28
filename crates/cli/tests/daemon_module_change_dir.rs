//! A daemon serving a module with `use chroot = no` enters the module root and
//! the client's destination beneath it the way rsync 3.5.2's `change_dir()`
//! does.
//!
//! - The module root: upstream: clientserver.c:1063 `change_dir(module_chdir)`
//!   for every session, the ownership walk of util1.c:1351-1361. A symlink in
//!   the module path owned by another user is refused; the client gets
//!   `@ERROR: chdir failed` (clientserver.c:692-699) and the daemon log gets
//!   `[Receiver] chdir <path> failed: <strerror> (<errno>)`.
//! - The destination beneath it: upstream: util1.c:1430-1446, the confined
//!   walk `secure_relative_dirfd()`. A symlink that leaves the module is
//!   refused with `ELOOP` whoever owns it; get_local_name() then prints
//!   `change_dir#N "<dest>" (in <module>) failed` and exits 3
//!   (main.c:776-781, :836-840, :865-869), or `mkdir ... failed` and exits 11
//!   for `--mkpath` (main.c:757-762, util1.c:231-266).
//! - `insecure links = yes` restores the plain chdir for both
//!   (syscall.c:139, :433-436; util1.c:1447-1451).
//!
//! Every cell asserts the rsync 3.5.2 result measured on glibc. When the 3.5.2
//! oracle resolves (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs with a 3.5.2 client, a
//! 3.5.2 daemon, or both, and all results must be equal - except the one
//! deliberate deviation below, which pins oc's server only.
//!
//! Deliberate deviation (owner-approved, like #8858 and #8899): a new
//! destination directory under a symlink that leaves the module. 3.5.2's
//! `do_mkdir()` (main.c:817) follows the link and makes the directory outside
//! the module before `change_dir#2` refuses it; oc refuses first with the
//! same `change_dir#2` line and exit code and makes nothing.
#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// `strerror(errno) (errno)`, the way upstream's rsyserr() renders a cause
/// (log.c:461-465), taken from this platform's own libc text.
fn errno_text(errno: i32) -> String {
    let rendered = std::io::Error::from_raw_os_error(errno).to_string();
    let text = rendered
        .strip_suffix(&format!(" (os error {errno})"))
        .unwrap_or(&rendered);
    format!("{text} ({errno})")
}

fn eloop() -> String {
    errno_text(rustix::io::Errno::LOOP.raw_os_error())
}

/// The program name a daemon prefixes its rsyserr lines with: `rsync` for
/// rsync 3.5.2, the branded `oc-rsync` for oc. Every other byte of a line is
/// compared as is.
fn program(server: &Path) -> &'static str {
    if server == oc_rsync_bin() {
        "oc-rsync"
    } else {
        "rsync"
    }
}

/// The daemon log's lines without the `YYYY/MM/DD hh:mm:ss [pid] ` stamp
/// (log.c:277-287), which no two runs share.
fn log_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .expect("read daemon log")
        .lines()
        .map(|line| {
            line.split_once("] ")
                .map_or(line, |(_, rest)| rest)
                .to_owned()
        })
        .collect()
}

/// upstream: syscall.c:560-565 - the ownership walk's refusal notice.
const UNTRUSTED_NOTICE: &str = "refusing to follow a symlink owned by an untrusted user; use \
     --insecure-links locally or \"insecure links = yes\" in a daemon module ONLY if every path \
     component is trusted";

/// A module `mod` holding an in-tree link `rel -> inner` and an absolute link
/// `esc` that leaves it, plus module `opt` on the same directory with
/// `insecure links = yes`.
struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let base = dir.path();
        for sub in ["src/d", "mod/inner", "outside"] {
            fs::create_dir_all(base.join(sub)).expect("mkdir");
        }
        fs::write(base.join("src/f"), b"one\n").expect("write source");
        fs::write(base.join("src/d/g"), b"two\n").expect("write source");
        symlink("inner", base.join("mod/rel")).expect("relative link");
        symlink(base.join("outside"), base.join("mod/esc")).expect("escaping link");
        Self { dir }
    }

    fn base(&self) -> &Path {
        self.dir.path()
    }

    fn log(&self) -> PathBuf {
        self.base().join("daemon.log")
    }

    /// The entries below `outside`, which no cell may create.
    fn outside_entries(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.base().join("outside"))
            .expect("list outside")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    /// A shell that starts `server` as a daemon whose config holds `modules`.
    fn daemon_shell(&self, server: &Path, modules: &str) -> PathBuf {
        let conf = self.base().join("rsyncd.conf");
        fs::write(
            &conf,
            format!("log file = {}\n{modules}", self.log().display()),
        )
        .expect("write config");
        let shell = self.base().join("daemon-rsh");
        fs::write(
            &shell,
            format!(
                "#!/bin/sh\nexec \"{}\" --config={} --server --daemon .\n",
                server.display(),
                conf.display()
            ),
        )
        .expect("write daemon shell");
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod shell");
        shell
    }

    fn modules(&self) -> String {
        let root = self.base().join("mod");
        format!(
            "[mod]\npath = {root}\nuse chroot = no\nread only = no\n\
             [opt]\npath = {root}\nuse chroot = no\nread only = no\ninsecure links = yes\n",
            root = root.display()
        )
    }

    /// Pushes `source` to `host::<destination>` and returns the exit code with
    /// stderr.
    fn push(
        &self,
        client: &Path,
        server: &Path,
        modules: &str,
        options: &[&str],
        source: &str,
        destination: &str,
    ) -> (Option<i32>, String) {
        let output = Command::new(client)
            .args(options)
            .arg("-e")
            .arg(self.daemon_shell(server, modules))
            .arg(self.base().join(source))
            .arg(format!("h::{destination}"))
            .output()
            .expect("run rsync");
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

/// The client/server pairs a cell runs: oc at both ends, then every pairing
/// with 3.5.2 when its binary resolves.
fn pairs() -> Vec<(String, PathBuf, PathBuf)> {
    let oc = oc_rsync_bin();
    let mut pairs = vec![("oc to oc".to_owned(), oc.clone(), oc.clone())];
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        let reference = upstream.binary().to_path_buf();
        pairs.push((
            "rsync 3.5.2 to rsync 3.5.2".to_owned(),
            reference.clone(),
            reference.clone(),
        ));
        pairs.push((
            "rsync 3.5.2 client, oc daemon".to_owned(),
            reference.clone(),
            oc.clone(),
        ));
        pairs.push(("oc client, rsync 3.5.2 daemon".to_owned(), oc, reference));
    }
    pairs
}

/// One refused destination beneath the module root.
struct TailCell {
    name: &'static str,
    options: &'static [&'static str],
    source: &'static str,
    destination: &'static str,
    code: i32,
    /// The line up to the strerror text.
    line: &'static str,
}

const TAIL_CELLS: [TailCell; 3] = [
    TailCell {
        name: "existing directory through an escaping link",
        options: &["-r"],
        source: "src/",
        destination: "mod/esc/",
        code: 3,
        line: "[Receiver] change_dir#1 \"esc/\" (in mod) failed: ",
    },
    TailCell {
        name: "single file whose parent is an escaping link",
        options: &[],
        source: "src/f",
        destination: "mod/esc/f",
        code: 3,
        line: "[Receiver] change_dir#3 \"esc\" (in mod) failed: ",
    },
    TailCell {
        name: "--mkpath below an escaping link",
        options: &["-r", "--mkpath"],
        source: "src/",
        destination: "mod/esc/a/b/",
        code: 11,
        line: "[Receiver] mkdir \"esc/a/b/\" (in mod) failed: ",
    },
];

/// The trailer whose code names the refusal; its source location is the
/// binary's own.
fn trailer(code: i32) -> String {
    let what = match code {
        3 => "errors selecting input/output files, dirs",
        11 => "error in file IO",
        _ => unreachable!("no cell exits {code}"),
    };
    format!("error: {what} (code {code}) at ")
}

#[test]
fn a_daemon_refuses_a_destination_that_leaves_the_module() {
    for cell in &TAIL_CELLS {
        for (pair, client, server) in pairs() {
            let fixture = Fixture::new();
            let modules = fixture.modules();
            let (code, stderr) = fixture.push(
                &client,
                &server,
                &modules,
                cell.options,
                cell.source,
                cell.destination,
            );
            let case = format!("{} / {pair}", cell.name);
            assert_eq!(code, Some(cell.code), "{case}: {stderr}");
            let line = format!("{}{}", cell.line, eloop());
            assert!(
                stderr.contains(&line),
                "{case}: expected {line:?} in {stderr}"
            );
            assert!(
                stderr.contains(&trailer(cell.code)),
                "{case}: expected the code-{} trailer in {stderr}",
                cell.code
            );
            assert!(
                fixture.outside_entries().is_empty(),
                "{case}: the daemon wrote outside the module: {:?}",
                fixture.outside_entries()
            );
            // upstream: log.c:390-407 - the daemon logs the cause and the
            // trailer. The cause is compared whole; the trailer up to its
            // source location, which names each binary's own file.
            let log = log_lines(&fixture.log());
            let prog = program(&server);
            let cause = format!("{prog}: {line}");
            assert!(
                log.contains(&cause),
                "{case}: expected {cause:?} in the log: {log:#?}"
            );
            let trailer = format!("{prog} {}", trailer(cell.code));
            assert!(
                log.iter().any(|logged| logged.starts_with(&trailer)),
                "{case}: expected {trailer:?} in the log: {log:#?}"
            );
        }
    }
}

/// Deliberate deviation: 3.5.2 makes `outside/new` and then refuses it; oc
/// refuses before the mkdir. The line and exit code are 3.5.2's, so only oc's
/// daemon is pinned here.
#[test]
fn a_daemon_refuses_a_new_directory_under_an_escaping_link_before_making_it() {
    let oc = oc_rsync_bin();
    let mut clients = vec![("oc client".to_owned(), oc.clone())];
    if let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) {
        clients.push((
            "rsync 3.5.2 client".to_owned(),
            upstream.binary().to_path_buf(),
        ));
    }
    for (pair, client) in clients {
        let fixture = Fixture::new();
        let modules = fixture.modules();
        let (code, stderr) = fixture.push(&client, &oc, &modules, &["-r"], "src/", "mod/esc/new/");
        let line = format!(
            "[Receiver] change_dir#2 \"esc/new\" (in mod) failed: {}",
            eloop()
        );
        assert_eq!(code, Some(3), "{pair}: {stderr}");
        assert!(
            stderr.contains(&line),
            "{pair}: expected {line:?} in {stderr}"
        );
        assert!(
            fixture.outside_entries().is_empty(),
            "{pair}: the daemon made {:?} outside the module",
            fixture.outside_entries()
        );
    }
}

/// An in-tree relative link is followed, and `insecure links = yes` follows
/// the escaping one: both transfers complete where the link points.
#[test]
fn a_daemon_follows_an_in_tree_link_and_an_opted_out_escape() {
    for (pair, client, server) in pairs() {
        let fixture = Fixture::new();
        let modules = fixture.modules();
        let (code, stderr) = fixture.push(&client, &server, &modules, &["-r"], "src/", "mod/rel/");
        assert_eq!(code, Some(0), "in-tree link / {pair}: {stderr}");
        assert_eq!(
            fs::read(fixture.base().join("mod/inner/d/g")).expect("read through rel"),
            b"two\n",
            "in-tree link / {pair}"
        );

        let (code, stderr) = fixture.push(&client, &server, &modules, &["-r"], "src/", "opt/esc/");
        assert_eq!(code, Some(0), "insecure links / {pair}: {stderr}");
        assert_eq!(
            fs::read(fixture.base().join("outside/d/g")).expect("read through esc"),
            b"two\n",
            "insecure links / {pair}"
        );
    }
}

/// A module root `flink -> real` owned by another user, as module `flink`
/// and, with `insecure links = yes`, as module `optflink`.
///
/// Changing a symlink's owner needs `sudo -n chown -h`; a host without it
/// fails the cells that use this rather than skipping them.
#[cfg(target_os = "linux")]
fn foreign_root(fixture: &Fixture) -> (PathBuf, String) {
    let base = fixture.base();
    fs::create_dir(base.join("real")).expect("mkdir real");
    let link = base.join("flink");
    symlink(base.join("real"), &link).expect("root link");
    let status = Command::new("sudo")
        .args(["-n", "chown", "-h", "nobody"])
        .arg(&link)
        .status()
        .expect("run sudo");
    assert!(
        status.success(),
        "this cell needs passwordless `sudo -n chown -h nobody`: {status}"
    );
    let modules = format!(
        "[flink]\npath = {link}\nuse chroot = no\nread only = no\n\
         [optflink]\npath = {link}\nuse chroot = no\nread only = no\ninsecure links = yes\n",
        link = link.display()
    );
    (link, modules)
}

/// The module root is a symlink owned by another user: the session is
/// refused before the transfer, and the daemon logs the walk's notice and the
/// chdir that failed, each line equal to 3.5.2's but for the program name.
#[cfg(target_os = "linux")]
#[test]
fn a_daemon_refuses_a_module_root_symlink_owned_by_another_user() {
    for (pair, client, server) in pairs() {
        let fixture = Fixture::new();
        let (link, modules) = foreign_root(&fixture);

        let (code, stderr) = fixture.push(&client, &server, &modules, &["-r"], "src/", "flink/");
        assert_eq!(code, Some(5), "{pair}: {stderr}");
        assert!(
            stderr.contains("@ERROR: chdir failed"),
            "{pair}: expected @ERROR: chdir failed in {stderr}"
        );
        assert!(
            fs::read_dir(fixture.base().join("real"))
                .expect("list real")
                .next()
                .is_none(),
            "{pair}: the daemon wrote through the refused root"
        );
        // upstream: clientserver.c:695 - rsyserr(FLOG, errno, "chdir %s failed"),
        // after the walk's own notice.
        let log = log_lines(&fixture.log());
        let chdir = format!(
            "{}: [Receiver] chdir {} failed: {}",
            program(&server),
            link.display(),
            eloop()
        );
        let notice = log.iter().position(|line| line == UNTRUSTED_NOTICE);
        let failed = log.iter().position(|line| *line == chdir);
        assert!(
            matches!((notice, failed), (Some(n), Some(f)) if n < f),
            "{pair}: expected {UNTRUSTED_NOTICE:?} then {chdir:?} in the log: {log:#?}"
        );
    }
}

/// `insecure links = yes` follows the same foreign-owned root, as 3.5.2 does:
/// the symlink is still there afterward and the data lands in its target.
/// oc used to replace the symlink with an empty directory and write into
/// that, losing the operator's link.
#[cfg(target_os = "linux")]
#[test]
fn an_opted_out_daemon_writes_through_a_foreign_root_symlink_and_keeps_it() {
    for (pair, client, server) in pairs() {
        let fixture = Fixture::new();
        let (link, modules) = foreign_root(&fixture);

        let (code, stderr) = fixture.push(&client, &server, &modules, &["-r"], "src/", "optflink/");
        assert_eq!(code, Some(0), "{pair}: {stderr}");
        let kept = fs::symlink_metadata(&link).expect("lstat root");
        assert!(
            kept.file_type().is_symlink(),
            "{pair}: the root symlink was replaced"
        );
        assert_eq!(
            fs::read_link(&link).expect("readlink root"),
            fixture.base().join("real"),
            "{pair}: the root symlink was retargeted"
        );
        assert_eq!(
            fs::read(fixture.base().join("real/d/g")).expect("read the link target"),
            b"two\n",
            "{pair}: the data did not land in the link target"
        );
    }
}
