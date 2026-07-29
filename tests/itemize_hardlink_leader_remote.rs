//! Itemized hardlink-follower rows must name their leader over a remote shell.
//!
//! Upstream renders the `%L` suffix of the default `-i` format (`%i %n%L`) as
//! ` => <leader>` for a file hard-linked to an earlier-transferred entry:
//!
//! ```c
//! /* log.c:643-646 - the hlink form wins over the symlink arrow */
//! case 'L':
//!         if (hlink && *hlink) {
//!                 n = hlink;
//!                 strlcpy(buf2, " => ", sizeof buf2);
//!
//! /* hlink.c:232-234 - each linked follower itemizes with the leader name */
//! itemize(fname, file, ndx, statret, sxp,
//!         ITEM_LOCAL_CHANGE | ITEM_XNAME_FOLLOWS, 0,
//!         realname);
//! ```
//!
//! Only followers carry the name: the first-of-set is transferred as a plain
//! `>f+++++++++` / `<f+++++++++` row with no ` => ` suffix. Verified against
//! rsync 3.4.4 (protocol 32): a 3-link set itemizes as one transfer row plus
//! two `hf+++++++++ <follower> => <leader>` rows in every transport.
//!
//! WHY this lives at the binary level: on a pull the client receiver renders
//! the follower rows itself (`create_hardlinks`), while on a push they travel
//! as wire iflags + xname and the client sender renders them. A unit test can
//! only see one renderer; this file exercises both roles for real, on whichever
//! receiver driver (`incremental-flist` on/off) the build selected.
//!
//! Skip conditions (test passes with a printed reason):
//! - Not Unix (the shim uses `/bin/sh`).

#![cfg(unix)]

use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(120);

/// Locates the binary under test.
///
/// `CARGO_BIN_EXE_oc-rsync` is a COMPILE-time variable, so it must be read with
/// `env!`, not `env::var_os`: at run time it is unset and the lookup would fall
/// through to whatever stale `target/debug/oc-rsync` happens to be on disk -
/// silently testing a different build than the one just compiled.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    for profile in ["debug", "release", "dist"] {
        let candidate = PathBuf::from(manifest_dir)
            .join("target")
            .join(profile)
            .join("oc-rsync");
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from("oc-rsync")
}

/// Writes the `--rsh` shim: drops the SSH-style options and host placeholder,
/// then execs the server command locally over a pipe pair.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    let body = "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n";
    fs::write(&script, body).expect("write rsh shim");
    let mut perms = fs::metadata(&script).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).unwrap();
    script
}

fn spawn_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // Drain both pipes on their own threads so the child never blocks writing
    // into a full OS pipe buffer while we poll try_wait().
    let mut child_stdout = child.stdout.take()?;
    let mut child_stderr = child.stderr.take()?;
    let stdout_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = child_stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().ok()? {
            Some(status) => {
                return Some(Output {
                    status,
                    stdout: stdout_reader.join().unwrap_or_default(),
                    stderr: stderr_reader.join().unwrap_or_default(),
                });
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    }
}

/// Lays out a 3-link hardlink set (`aaa` sorts first and becomes the leader on
/// oc's receiving side) plus an empty destination.
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::TempDir::new().expect("create temp dir");
    let root = temp.path();
    let src = root.join("src");
    let dest = root.join("dest");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dest).unwrap();
    fs::write(src.join("aaa"), b"shared hardlink payload\n").unwrap();
    fs::hard_link(src.join("aaa"), src.join("bbb")).unwrap();
    fs::hard_link(src.join("aaa"), src.join("ccc")).unwrap();
    (temp, src, dest)
}

/// Runs `-aHi` over the shim in the requested direction.
fn run_transfer(shim: &Path, binary: &Path, src: &Path, dest: &Path, push: bool) -> Output {
    let (from, to) = if push {
        (
            format!("{}/", src.display()),
            format!("hlhost:{}/", dest.display()),
        )
    } else {
        (
            format!("hlhost:{}/", src.display()),
            format!("{}/", dest.display()),
        )
    };
    let mut cmd = Command::new(binary);
    cmd.arg("-aHi")
        .arg("--rsh")
        .arg(shim)
        .arg("--rsync-path")
        .arg(binary)
        .arg(from)
        .arg(to);
    spawn_with_timeout(cmd, RUN_TIMEOUT).expect("transfer did not finish within the timeout")
}

/// Asserts one direction: the transferred first-of-set row carries no ` => `
/// suffix, every `hf` follower row names the leader, and the destination set
/// really shares one inode.
fn assert_follower_rows(push: bool) {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let (temp, src, dest) = setup();
    let shim = write_rsh_shim(temp.path());

    let output = run_transfer(&shim, &binary, &src, &dest, push);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let label = if push { "push" } else { "pull" };

    assert!(
        output.status.success(),
        "{label} must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    // upstream: the first-of-set is a plain transfer row - no hardlink suffix.
    let direction = if push { '<' } else { '>' };
    let leader_row = format!("{direction}f+++++++++ aaa");
    assert!(
        stdout.lines().any(|line| line == leader_row),
        "{label}: first-of-set must itemize as {leader_row:?} with no ` => `\
         suffix; got:\n{stdout}"
    );

    // upstream: hlink.c:232-234 + log.c:643-646 - each follower renders
    // `hf+++++++++ <name> => <leader>`.
    for follower in ["bbb", "ccc"] {
        let follower_row = format!("hf+++++++++ {follower} => aaa");
        assert!(
            stdout.lines().any(|line| line == follower_row),
            "{label}: follower must itemize as {follower_row:?}; got:\n{stdout}"
        );
    }

    // The pre-fix symptom was a bare `hf` row: `ITEM_XNAME_FOLLOWS` set but the
    // leader name dropped. Guard against any suffix-less follower row.
    for line in stdout.lines() {
        if line.starts_with("hf") {
            assert!(
                line.contains(" => "),
                "{label}: every hf row must carry the ` => leader` suffix,\
                 got {line:?} in:\n{stdout}"
            );
        }
    }

    // The rows must describe a real hardlink set at the destination.
    {
        use std::os::unix::fs::MetadataExt;
        let ino_a = fs::metadata(dest.join("aaa")).unwrap().ino();
        for follower in ["bbb", "ccc"] {
            assert_eq!(
                fs::metadata(dest.join(follower)).unwrap().ino(),
                ino_a,
                "{label}: {follower} must share the leader's inode"
            );
        }
    }

    drop(temp);
}

#[test]
fn pull_hardlink_followers_itemize_with_leader_suffix() {
    assert_follower_rows(false);
}

#[test]
fn push_hardlink_followers_itemize_with_leader_suffix() {
    assert_follower_rows(true);
}
