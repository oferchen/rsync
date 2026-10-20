//! A `--delete` run that meets a busy mount point inside a doomed directory
//! reports it and keeps going, the way upstream does.
//!
//! upstream: delete.c:141-164 `delete_dir_contents` deletes every sibling and
//! marks the directory `DR_NOT_EMPTY` when one child fails; delete.c:264-266
//! prints `delete_file: rmdir(%s) failed` for the `EBUSY`; the pass never
//! aborts, so the transfer itself still runs and ends `RERR_PARTIAL` (23).
//! Without `-x` the mount is not a boundary, so its contents are deleted too.
//!
//! The fixture needs a real mount. The tests mount a tmpfs (directly as root,
//! else through `sudo -n`) and skip loudly when neither is allowed.
#![cfg(target_os = "linux")]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

fn privileged(args: &[&str]) -> Command {
    let is_root = Command::new("id")
        .arg("-u")
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0");
    let mut command = if is_root {
        Command::new(args[0])
    } else {
        let mut sudo = Command::new("sudo");
        sudo.args(["-n", args[0]]);
        sudo
    };
    command.args(&args[1..]);
    command
}

/// Unmounts the tmpfs when the test ends, pass or fail.
struct Mounted(PathBuf);

impl Drop for Mounted {
    fn drop(&mut self) {
        let target = self.0.display().to_string();
        let _ = privileged(&["umount", &target]).status();
    }
}

/// Mounts a small tmpfs on `target`, or prints a SKIP notice and returns
/// `None` when this host does not allow it.
fn mount_tmpfs(target: &Path) -> Option<Mounted> {
    let target_arg = target.display().to_string();
    let mounted = privileged(&[
        "mount",
        "-t",
        "tmpfs",
        "-o",
        "size=1m,mode=0777",
        "tmpfs",
        &target_arg,
    ])
    .output()
    .is_ok_and(|out| out.status.success());
    if !mounted {
        eprintln!("SKIP: cannot mount a tmpfs on {target_arg} (needs root or passwordless sudo)");
        return None;
    }
    Some(Mounted(target.to_path_buf()))
}

struct Fixture {
    _temp: tempfile::TempDir,
    source: PathBuf,
    dest: PathBuf,
}

/// `dst/x` is extraneous and holds a file `sib` and a tmpfs mounted on `m`.
fn fixture() -> Option<(Fixture, Mounted)> {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&source).expect("mkdir src");
    fs::create_dir_all(dest.join("x/m")).expect("mkdir dst/x/m");
    fs::write(source.join("a"), b"a\n").expect("write a");
    fs::write(dest.join("x/sib"), b"old\n").expect("write sib");
    let mounted = mount_tmpfs(&dest.join("x/m"))?;
    fs::write(dest.join("x/m/inner"), b"i\n").expect("write inner");
    Some((
        Fixture {
            _temp: temp,
            source,
            dest,
        },
        mounted,
    ))
}

fn run(fixture: &Fixture, extra: &[&str]) -> Output {
    Command::new(oc_rsync_binary())
        .args(["-a", "--delete"])
        .args(extra)
        .arg(format!("{}/", fixture.source.display()))
        .arg(format!("{}/", fixture.dest.display()))
        .output()
        .expect("run oc-rsync")
}

fn assert_matches_upstream(fixture: &Fixture, output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let context = format!("stdout: {stdout}\nstderr: {stderr}");
    assert_eq!(output.status.code(), Some(23), "{context}");
    assert!(
        stderr.lines().any(|line| line
            == "rsync: [generator] delete_file: rmdir(x/m) failed: Device or resource busy (16)"),
        "the busy mount is reported as upstream 3.5.1 does: {context}"
    );
    assert!(
        stdout
            .lines()
            .any(|line| line == "cannot delete non-empty directory: x"),
        "the directory the mount pins is reported as non-empty: {context}"
    );
    assert!(
        !fixture.dest.join("x/sib").exists(),
        "the mount's sibling is still deleted: {context}"
    );
    assert!(
        !fixture.dest.join("x/m/inner").exists(),
        "without -x the mount's contents are deleted: {context}"
    );
    assert_eq!(
        fs::read(fixture.dest.join("a")).expect("the transfer still ran"),
        b"a\n",
        "{context}"
    );
}

#[test]
fn delete_steps_over_a_busy_mount_point() {
    let Some((fixture, _mounted)) = fixture() else {
        return;
    };
    let output = run(&fixture, &[]);
    assert_matches_upstream(&fixture, &output);
}

/// `--max-delete` routes through the capped leaf executor, which used to
/// abort the whole transfer on the `EBUSY`.
#[test]
fn capped_delete_steps_over_a_busy_mount_point() {
    let Some((fixture, _mounted)) = fixture() else {
        return;
    };
    let output = run(&fixture, &["--max-delete=10"]);
    assert_matches_upstream(&fixture, &output);
}
