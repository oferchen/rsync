//! A daemon must not read a client-requested `--files-from=:LIST` through a
//! trusted-owned symlink that leaves the module root.
//!
//! # Why the ownership walk is not enough here
//!
//! The list path comes from the CLIENT, but the daemon opens it. The ownership
//! walk deliberately FOLLOWS a symlink owned by uid 0 or our own euid, and a
//! non-chrooted daemon writes everything it creates (a `--backup-dir` entry,
//! say) as its own uid. So a symlink a writable module already contains is
//! trusted-owned by construction. Point it outside the module and a plain
//! open reads an out-of-module file as the list; every line then comes back to
//! the client as a `link_stat "<line>" (in m) failed` error. The file's
//! contents are disclosed one line at a time.
//!
//! Upstream closes this with the module-root half of the rule:
//! `operator_path_resolve = am_daemon ? 1 : 0` around the list open arms
//! `abspath_outside_confinement()` inside the walk. Off a daemon the same open
//! still runs the ownership walk, but without the module-root check, because
//! there the list path is the operator's own argument.
//!
//! # Deterministic, not a race
//!
//! The symlink is planted before the transfer starts. The escape only needs a
//! trusted-owned symlink to EXIST at the named path.
//!
//! # Why these cells run unprivileged
//!
//! Upstream refuses only `st_uid != 0 && st_uid != trusted_uid`
//! (`syscall.c:499`), so uid 0 and the euid take one identical follow path into
//! one identical confinement check. The daemon spawned here runs as the test's
//! own uid, so a plant owned by that uid is exactly the root-owned case a
//! privileged daemon would see.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.1/options.c:2652-2671` - `operator_path_resolve = am_daemon ?
//!   1 : 0;` around `open_no_attacker_symlinks(files_from, ...)`.
//! - `rsync-3.5.1/syscall.c:232-291` `abspath_outside_confinement()`.
#![cfg(unix)]
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use test_support::{LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, require_binaries};

/// The out-of-module file's single line. It names no file in the module, so a
/// leak surfaces as this text in the client's error output.
const SECRET: &str = "NO_ONE_SHOULD_README";
/// A file the module does contain, named by the legitimate list.
const WANTED: &str = "wanted.txt";
const WANTED_CONTENT: &str = "wanted-content\n";

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What the client asks the daemon to read as its `--files-from` list.
#[derive(Clone, Copy)]
enum Request {
    /// `list`, a trusted-owned symlink to a file OUTSIDE the module root.
    SymlinkOutside,
    /// `list`, a trusted-owned symlink to a file INSIDE the module root.
    SymlinkInside,
    /// The out-of-module file's own absolute path.
    AbsoluteOutside,
}

/// Whether the daemon worker installs oc's Landlock and seccomp layers.
///
/// Landlock refuses the out-of-module read on its own, BEFORE oc's
/// operator-path resolution is consulted, so the escape cell opts both layers
/// out to observe oc's own refusal. This is a TEST-ONLY narrowing; the shipped
/// daemon installs both layers by default. The availability cell keeps them on.
#[derive(Clone, Copy)]
enum Sandbox {
    Enforced,
    OptedOut,
}

impl Sandbox {
    fn daemon_env(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Enforced => &[],
            Self::OptedOut => &[("OC_RSYNC_NO_LANDLOCK", "1"), ("OC_RSYNC_NO_SECCOMP", "1")],
        }
    }
}

struct Outcome {
    client_exit: Option<i32>,
    client_stdout: String,
    client_stderr: String,
    daemon_log: String,
    /// Every name the pull created under the destination.
    received: Vec<String>,
    wanted: io::Result<String>,
}

impl Outcome {
    fn diagnostics(&self) -> String {
        format!(
            "\nexit: {:?}\nstdout:\n{}\nstderr:\n{}\ndaemon log:\n{}",
            self.client_exit, self.client_stdout, self.client_stderr, self.daemon_log
        )
    }
}

fn write_config(config: &Path, module_root: &Path, log_root: &Path) -> io::Result<()> {
    fs::write(
        config,
        format!(
            "pid file = {pid}\n\
             log file = {log}\n\
             use chroot = false\n\
             \n\
             [data]\n\
             path = {root}\n\
             read only = true\n",
            pid = log_root.join("rsyncd.pid").display(),
            log = log_root.join("rsyncd.log").display(),
            root = module_root.display(),
        ),
    )
}

fn spawn_daemon(
    oc_bin: &Path,
    config: &Path,
    cwd: &Path,
    sandbox: Sandbox,
) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        let mut cmd = Command::new(oc_bin);
        cmd.current_dir(cwd)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in sandbox.daemon_env() {
            cmd.env(key, value);
        }
        cmd.spawn()
    })?;
    Ok((DaemonGuard(child), port))
}

/// Stages a module whose `list` is a trusted-owned symlink, pulls with the
/// `--files-from=:` value `request` names, and reports what the client saw and
/// received.
fn pull_with_module_list(request: Request, sandbox: Sandbox) -> Outcome {
    let oc_bin = test_support::oc_rsync_bin();
    let tmp = test_support::create_tempdir();
    let root = tmp.path();
    let module_root = root.join("module");
    let outside_dir = root.join("outside");
    let dest = root.join("dest");
    fs::create_dir_all(module_root.join("lists")).expect("create module lists dir");
    fs::create_dir_all(&outside_dir).expect("create the out-of-module dir");
    fs::create_dir_all(&dest).expect("create destination");

    fs::write(module_root.join(WANTED), WANTED_CONTENT).expect("seed wanted file");
    let inside_list = module_root.join("lists").join("in-tree");
    fs::write(&inside_list, format!("{WANTED}\n")).expect("seed in-module list");
    let outside_list = outside_dir.join("secret");
    fs::write(&outside_list, format!("{SECRET}\n")).expect("seed out-of-module file");

    let list = module_root.join("list");
    let list_target = match request {
        Request::SymlinkInside => &inside_list,
        Request::SymlinkOutside | Request::AbsoluteOutside => &outside_list,
    };
    symlink(list_target, &list).expect("plant the list symlink");
    // The plant must be TRUSTED-owned, or a refusal would only be the ownership
    // arm firing and would prove nothing about the module root.
    let meta = fs::symlink_metadata(&list).expect("the list symlink must exist");
    assert!(
        fast_io::symlink_owner_is_trusted(meta.uid()),
        "the planted list must be owned by uid 0 or our euid; got uid {}",
        meta.uid()
    );

    let config = root.join("rsyncd.conf");
    write_config(&config, &module_root, root).expect("write daemon config");
    // The escape cell starts the daemon from INSIDE the module, which is the
    // cwd upstream serves from (clientserver.c:1059). There a relative list
    // reaches the plant even without anchoring, so only the confinement can
    // stop the read. The other cells start it from outside, so a relative list
    // that is not anchored at the module misses its file.
    let daemon_cwd = match request {
        Request::SymlinkOutside => module_root.as_path(),
        Request::SymlinkInside | Request::AbsoluteOutside => root,
    };
    let (_daemon, port) = spawn_daemon(&oc_bin, &config, daemon_cwd, sandbox)
        .expect("start the daemon, or nothing is measured");

    let files_from = match request {
        Request::SymlinkOutside | Request::SymlinkInside => "list".to_owned(),
        Request::AbsoluteOutside => outside_list.display().to_string(),
    };
    let output = Command::new(&oc_bin)
        .arg("-r")
        .arg(format!("--files-from=:{files_from}"))
        .arg(format!("rsync://127.0.0.1:{port}/data/"))
        .arg(format!("{}/", dest.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync client");

    let mut received: Vec<String> = fs::read_dir(&dest)
        .expect("read destination")
        .map(|entry| {
            entry
                .expect("destination entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    received.sort();
    Outcome {
        client_exit: output.status.code(),
        client_stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        client_stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        daemon_log: fs::read_to_string(root.join("rsyncd.log")).unwrap_or_default(),
        received,
        wanted: fs::read_to_string(dest.join(WANTED)),
    }
}

/// Asserts that nothing read from the out-of-module file reached the client.
fn assert_not_disclosed(outcome: &Outcome, what: &str) {
    // NON-VACUITY: a session that died before the list open would also leak
    // nothing. The daemon logs the module request once it has accepted it.
    assert!(
        outcome.daemon_log.contains("data"),
        "{what}: the daemon never served the module, so the assertions below \
         would hold vacuously{}",
        outcome.diagnostics(),
    );
    assert!(
        !outcome.client_stderr.contains(SECRET) && !outcome.client_stdout.contains(SECRET),
        "{what}: the out-of-module file was read as the files-from list and \
         its contents reached the client{}",
        outcome.diagnostics(),
    );
    assert!(
        !outcome.received.iter().any(|name| name.contains(SECRET)),
        "{what}: the destination received an entry named by the out-of-module \
         file: {:?}{}",
        outcome.received,
        outcome.diagnostics(),
    );
    assert_ne!(
        outcome.client_exit,
        Some(0),
        "{what}: the daemon must refuse the list, not succeed with an empty \
         transfer{}",
        outcome.diagnostics(),
    );
}

/// THE PIN. Without the module-root check the daemon reads the out-of-module
/// file as the list and echoes its line back in a `link_stat` error.
#[test]
fn a_daemon_files_from_symlink_leaving_the_module_is_refused() {
    let outcome = pull_with_module_list(Request::SymlinkOutside, Sandbox::OptedOut);
    assert_not_disclosed(&outcome, "out-of-module list symlink");
}

/// The same disclosure without any symlink: an absolute value must re-root at
/// the module (upstream `options.c:2641-2642` `sanitize_path()`), never name a
/// path on the daemon's filesystem.
#[test]
fn an_absolute_daemon_files_from_cannot_name_a_file_outside_the_module() {
    let outcome = pull_with_module_list(Request::AbsoluteOutside, Sandbox::OptedOut);
    assert_not_disclosed(&outcome, "absolute out-of-module list");
}

/// POSITIVE CONTROL for over-refusal, and the harness proof for the pin: the
/// same fixture with the symlink pointing INSIDE the module must still be
/// followed. The confinement applies to the landing site, not to symlinks.
#[test]
fn a_daemon_files_from_symlink_inside_the_module_is_followed() {
    let outcome = pull_with_module_list(Request::SymlinkInside, Sandbox::Enforced);
    assert_eq!(
        outcome.client_exit,
        Some(0),
        "an in-module list symlink must not fail the transfer{}",
        outcome.diagnostics(),
    );
    assert_eq!(
        outcome.wanted.as_deref().ok(),
        Some(WANTED_CONTENT),
        "the file named by the in-module list must arrive{}",
        outcome.diagnostics(),
    );
}

/// Off a daemon the list path is the operator's own argument, so upstream runs
/// only the ownership walk (`operator_path_resolve = 0`): a trusted-owned
/// symlink to a list anywhere is followed. A fix that confined every server
/// list open would pass both daemon cells and break this one.
#[test]
fn an_rsh_server_files_from_symlink_is_not_module_confined() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = test_support::create_tempdir();
    let root = tmp.path();
    let src = root.join("src");
    let lists = root.join("lists");
    let dest = root.join("dest");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&lists).expect("mkdir lists");
    fs::create_dir_all(&dest).expect("mkdir dest");
    fs::write(src.join(WANTED), WANTED_CONTENT).expect("seed wanted file");
    let real_list = lists.join("real");
    fs::write(&real_list, format!("{WANTED}\n")).expect("seed list");
    let list = src.join("list");
    symlink(&real_list, &list).expect("plant the list symlink");

    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let output = OcRsyncCliRunner::new()
        .arg("-r")
        .arg(format!("--files-from=:{}", list.display()))
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("pull run");
    assert_eq!(
        output.status,
        Some(0),
        "a non-daemon server must follow a trusted-owned list symlink\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        fs::read_to_string(dest.join(WANTED)).ok().as_deref(),
        Some(WANTED_CONTENT),
        "the file named by the out-of-tree list must arrive",
    );
}
