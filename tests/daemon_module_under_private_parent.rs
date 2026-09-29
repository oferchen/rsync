//! A root daemon serves a module whose PARENT the drop identity cannot search.
//!
//! Upstream enters the module before it drops privileges: `change_dir()` into
//! the module (clientserver.c:1059) and `module_dirfd = open(".")` (:1065) both
//! run above the `setgid()`/`setuid()` at clientserver.c:1098/1123, and the
//! receiver then works with names relative to that working directory. A module
//! owned by the drop identity (`nobody` for a root daemon with no `uid =`) is
//! therefore served even when it sits under a 0700 directory `nobody` cannot
//! traverse, e.g. `path = /home/backup/data`.
//!
//! A receiver that re-resolves the module's absolute path after the drop fails
//! that ancestor search with `EACCES`. The push then either aborts or, worse,
//! finishes while silently skipping the root's attributes, so the test checks
//! the exit code AND that the root and the transferred entries really landed.
//!
//! Needs root: it runs directly as root, or through `sudo -n`, and skips with a
//! printed reason otherwise.
#![cfg(target_os = "linux")]
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn running_as_root() -> bool {
    Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "0")
}

/// The command prefix that runs a program as root, or `None` when neither
/// running as root nor holding passwordless `sudo`.
fn as_root() -> Option<Vec<&'static str>> {
    if running_as_root() {
        return Some(Vec::new());
    }
    let sudo_ok = Command::new("sudo")
        .args(["-n", "true"])
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    sudo_ok.then(|| vec!["sudo", "-n"])
}

fn root_cmd(prefix: &[&str], program: &str) -> Command {
    match prefix.split_first() {
        Some((first, rest)) => {
            let mut cmd = Command::new(first);
            cmd.args(rest).arg(program);
            cmd
        }
        None => Command::new(program),
    }
}

fn run_as_root(prefix: &[&str], program: &str, args: &[&str]) {
    let status = root_cmd(prefix, program)
        .args(args)
        .status()
        .expect("spawn privileged command");
    assert!(status.success(), "{program} {args:?} failed: {status}");
}

fn backdate(path: &Path) {
    let status = Command::new("touch")
        .args(["-t", "202001010000"])
        .arg(path)
        .status()
        .expect("spawn touch");
    assert!(status.success(), "touch failed: {status}");
}

/// Stops the root daemon and hands the module back so the temp dir can go.
struct Cleanup {
    prefix: Vec<&'static str>,
    pid_file: PathBuf,
    module: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Ok(pid) = fs::read_to_string(&self.pid_file) {
            let _ = root_cmd(&self.prefix, "kill").arg(pid.trim()).status();
        }
        let owner = format!("{}:{}", nix_like_id("-u"), nix_like_id("-g"));
        let _ = root_cmd(&self.prefix, "chown")
            .args(["-R", &owner])
            .arg(&self.module)
            .status();
    }
}

fn nix_like_id(flag: &str) -> String {
    let out = Command::new("id").arg(flag).output().expect("spawn id");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn wait_for_port(port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Which push a cell makes into the module.
#[derive(Clone, Copy, Debug)]
enum Cell {
    /// A plain `-a` push into an empty module.
    Plain,
    /// `-a --delete --backup --backup-dir --temp-dir` into a populated module,
    /// so the delete, backup and temp-file lookups all run after the drop.
    DeleteBackupTempDir,
}

fn run_cell(cell: Cell) {
    let Some(prefix) = as_root() else {
        eprintln!("skip: needs root or passwordless sudo to run a root daemon");
        return;
    };
    // tempfile creates the directory 0700, owned by the test user: exactly the
    // parent the daemon's `nobody` identity cannot search.
    let temp = tempfile::tempdir().expect("tempdir");
    let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("chmod parent");
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).expect("create src");
    fs::write(src.join("f"), b"payload\n").expect("write src file");
    backdate(&src.join("f"));
    backdate(&src.join("sub"));
    backdate(&src);
    // The client reads `src/` as the test user; only the module is `nobody`'s.
    let module = root.join("mod");
    fs::create_dir(&module).expect("create module");
    fs::set_permissions(&module, fs::Permissions::from_mode(0o755)).expect("chmod module");
    if matches!(cell, Cell::DeleteBackupTempDir) {
        fs::write(module.join("f"), b"old\n").expect("seed module file");
        fs::write(module.join("stale"), b"gone\n").expect("seed stale file");
        fs::create_dir(module.join("bak")).expect("create backup dir");
        fs::create_dir(module.join("tmpd")).expect("create temp dir");
    }
    run_as_root(
        &prefix,
        "chown",
        &["-R", "nobody:", module.to_str().expect("utf-8 path")],
    );

    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("free port")
        .port();
    // The pid file must be writable by the root daemon before it drops.
    let pid_file = std::env::temp_dir().join(format!("oc-private-parent-{port}.pid"));
    let conf = std::env::temp_dir().join(format!("oc-private-parent-{port}.conf"));
    fs::write(
        &conf,
        format!(
            "use chroot = no\npid file = {}\n[m]\n\tpath = {}\n\tread only = no\n",
            pid_file.display(),
            module.display()
        ),
    )
    .expect("write config");
    let _cleanup = Cleanup {
        prefix: prefix.clone(),
        pid_file: pid_file.clone(),
        module: module.clone(),
    };
    let binary = oc_binary();
    let _daemon = root_cmd(&prefix, binary.to_str().expect("utf-8 path"))
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--port={port}"))
        .arg(format!("--config={}", conf.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn root daemon");
    assert!(
        wait_for_port(port),
        "the root daemon never listened on {port}"
    );

    let mut push = Command::new(&binary);
    push.arg("-a").arg("--timeout=20");
    if matches!(cell, Cell::DeleteBackupTempDir) {
        push.args([
            "--delete",
            "--backup",
            "--backup-dir=bak",
            "--temp-dir=tmpd",
            "--exclude=/bak",
            "--exclude=/tmpd",
        ]);
    }
    let out = push
        .arg(format!("{}/", src.display()))
        .arg(format!("rsync://127.0.0.1:{port}/m/"))
        .output()
        .expect("run push");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{cell:?}: upstream enters the module before the privilege drop, so a \
         0700 parent must not stop the push\nstderr:\n{stderr}"
    );
    // Reading back needs root too: the module lies under the test user's
    // 0700 dir, which is fine, but assert on stat only.
    let want = fs::metadata(&src).expect("stat src").mtime();
    assert_eq!(
        fs::metadata(&module).expect("stat module").mtime(),
        want,
        "the module root's mtime must be applied, not silently skipped"
    );
    assert_eq!(
        fs::metadata(module.join("sub")).expect("stat sub").mtime(),
        fs::metadata(src.join("sub")).expect("stat src sub").mtime(),
        "a nested directory's mtime must be applied"
    );
    assert_eq!(
        fs::read(module.join("f")).expect("read transferred file"),
        b"payload\n"
    );
    assert_eq!(
        fs::metadata(module.join("f")).expect("stat f").mtime(),
        fs::metadata(src.join("f")).expect("stat src f").mtime(),
        "{cell:?}: the file's mtime must be applied"
    );
    if matches!(cell, Cell::DeleteBackupTempDir) {
        assert!(
            !module.join("stale").exists(),
            "--delete must remove the stale file"
        );
        assert_eq!(
            fs::read(module.join("bak").join("f")).expect("read backup"),
            b"old\n",
            "--backup-dir must keep the replaced file"
        );
        assert_eq!(
            fs::read(module.join("bak").join("stale")).expect("read deleted backup"),
            b"gone\n",
            "--backup-dir must keep the deleted file"
        );
    }
    let _ = fs::remove_file(&conf);
}

#[test]
fn root_daemon_serves_a_module_under_a_parent_nobody_cannot_search() {
    run_cell(Cell::Plain);
}

#[test]
fn delete_backup_and_temp_dir_work_under_a_parent_nobody_cannot_search() {
    run_cell(Cell::DeleteBackupTempDir);
}
