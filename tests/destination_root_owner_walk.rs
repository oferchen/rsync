//! CVE-2026-53796: every non-daemon receiver enters its destination through
//! the ownership walk and keeps the directory it entered.
//!
//! Upstream's receiver `change_dir()`s into the destination with
//! `open_no_attacker_symlinks_dirfd()`, which follows a component symlink only
//! when root or the euid owns it, and then `fchdir()`s to the descriptor it
//! returns. Two observable consequences, both pinned here for the local copy,
//! the `--server` receiver of a push and the client receiver of a pull:
//!
//! - a destination symlink owned by another uid is refused with exit 3
//!   (`RERR_FILESELECT`) and upstream's actionable diagnostic, and nothing is
//!   written where it points; the operator's own, a root-owned one, a real
//!   directory, and anything under `--insecure-links` are followed as before;
//! - a destination swapped for a symlink after it was entered cannot redirect
//!   the rest of the transfer: every later write lands in the directory the
//!   walk accepted.
//!
//! The operand always carries a trailing slash (`dest/sub/`), which resolves a
//! final symlink in the kernel, so the refusal cells also prove the walk sees
//! the bare name. Planting a symlink owned by another uid needs root; the
//! cells use `sudo -n` when not root and report a loud skip when neither is
//! available.
//!
//! upstream: `rsync-3.5.1/util1.c:1363-1393` and `:1450-1466` `change_dir()`,
//! `rsync-3.5.1/syscall.c:499-504` (the diagnostic), `rsync-3.5.1/main.c:778-781`
//! (`RERR_FILESELECT`), `rsync-3.5.1/NEWS.md:142-148`.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// uid 65534 is `nobody` on every supported Unix.
const UNTRUSTED_UID: u32 = 65534;
const REFUSAL: &str = "refusing to follow a symlink owned by an untrusted user";
const RUN_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug)]
enum Receiver {
    Local,
    Push,
    Pull,
}

const RECEIVERS: [Receiver; 3] = [Receiver::Local, Receiver::Push, Receiver::Pull];

/// `CARGO_BIN_EXE_oc-rsync` is a compile-time variable; reading it at run time
/// would fall through to a stale binary.
fn binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}",
        built.display()
    );
    built
}

/// A `--rsh` shim that drops the host argument and runs the rest locally the
/// way `ssh` does - joined into one shell command line - so a push reaches the
/// real `--server` receiver, a pull the client one, and a `--rsync-path`
/// carrying extra words splits into arguments.
fn rsh_shim(base: &Path) -> PathBuf {
    let shim = base.join("rsh.sh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec /bin/sh -c \"$*\"\n").expect("write rsh shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    shim
}

/// Builds `base/{src,dest,outside}`; `src` holds the files named in `files`.
fn fixture(files: &[(&str, usize)]) -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(temp.path()).expect("canonicalize");
    fs::create_dir_all(base.join("src")).expect("mkdir src");
    for (name, len) in files {
        let bytes: Vec<u8> = (0..*len).map(|i| (i % 251) as u8).collect();
        fs::write(base.join("src").join(name), bytes).expect("write source");
    }
    fs::create_dir_all(base.join("dest")).expect("mkdir dest");
    fs::create_dir_all(base.join("outside")).expect("mkdir outside");
    (temp, base)
}

/// The command for `receiver`, copying `src/` into the `dest/sub/` operand.
///
/// `relative` names the destination relative to `base` (the cwd), exercising
/// upstream's `curr_dir` arm. `server_args` reach only the remote side, which
/// is how a remote-shell peer is given a local-only option such as
/// `--insecure-links` (upstream: options.c:3077-3080 never forwards it).
fn command(
    receiver: Receiver,
    base: &Path,
    relative: bool,
    client_args: &[&str],
    server_args: &[&str],
) -> Command {
    let bin = binary();
    let dest = if relative {
        PathBuf::from("dest/sub/")
    } else {
        base.join("dest/sub/")
    };
    let src = format!("{}/", base.join("src").display());
    let mut cmd = Command::new(&bin);
    cmd.current_dir(base).arg("-rt").args(client_args);
    match receiver {
        Receiver::Local => {
            cmd.arg(src).arg(dest);
        }
        Receiver::Push | Receiver::Pull => {
            let mut rsync_path = bin.display().to_string();
            for arg in server_args {
                rsync_path.push(' ');
                rsync_path.push_str(arg);
            }
            cmd.arg("-e")
                .arg(rsh_shim(base))
                .arg(format!("--rsync-path={rsync_path}"));
            if matches!(receiver, Receiver::Push) {
                cmd.arg(src)
                    .arg(format!("localhost:{}", base.join("dest/sub/").display()));
            } else {
                cmd.arg(format!("localhost:{src}")).arg(dest);
            }
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn wait(mut child: std::process::Child) -> Output {
    let deadline = Instant::now() + RUN_TIMEOUT;
    while child.try_wait().expect("poll child").is_none() {
        assert!(
            Instant::now() < deadline,
            "oc-rsync did not exit within {RUN_TIMEOUT:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().expect("collect output")
}

fn run(cmd: &mut Command) -> Output {
    wait(cmd.spawn().expect("spawn oc-rsync"))
}

fn describe(output: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Chowns the symlink at `path` (not its target) to `uid`, as root directly
/// or through `sudo -n`. Returns `false` when neither is available.
fn chown_symlink(path: &Path, uid: u32) -> bool {
    // The link was just created by this process, so its owner is the euid.
    let running_as_root = fs::symlink_metadata(path).is_ok_and(|meta| meta.uid() == 0);
    if running_as_root {
        return std::os::unix::fs::lchown(path, Some(uid), Some(uid)).is_ok();
    }
    Command::new("sudo")
        .args(["-n", "chown", "-h", &format!("{uid}:{uid}")])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Plants `dest/sub -> outside`, owned by `uid` when given. `None` means the
/// ownership change needs privileges this host does not grant.
fn plant(base: &Path, uid: Option<u32>) -> Option<()> {
    let link = base.join("dest/sub");
    symlink(base.join("outside"), &link).expect("plant symlink");
    match uid {
        Some(uid) => chown_symlink(&link, uid).then_some(()),
        None => Some(()),
    }
}

fn skip(case: &str) {
    eprintln!(
        "SKIPPED {case}: planting a symlink owned by another uid needs root or passwordless sudo"
    );
}

fn assert_refused(receiver: Receiver, relative: bool) {
    let case = format!("{receiver:?} untrusted (relative={relative})");
    let (_temp, base) = fixture(&[("f0", 64), ("f1", 64)]);
    if plant(&base, Some(UNTRUSTED_UID)).is_none() {
        return skip(&case);
    }
    let output = run(&mut command(receiver, &base, relative, &[], &[]));
    assert_eq!(
        output.status.code(),
        Some(3),
        "{case}: {}",
        describe(&output)
    );
    // The walk prints its diagnostic in the receiving process. A `--server`
    // receiver's diagnostics are not yet relayed to the client (upstream sends
    // them as MSG_ERROR, log.c:357), so a push shows only the fatal error the
    // server returns; the refusal itself is the same walk either way.
    let expected = match receiver {
        Receiver::Push => "failed to access destination directory",
        Receiver::Local | Receiver::Pull => REFUSAL,
    };
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "{case}: expected {expected:?}: {}",
        describe(&output)
    );
    assert!(
        entries(&base.join("outside")).is_empty(),
        "{case}: the receiver wrote through the attacker's symlink"
    );
}

fn assert_followed(receiver: Receiver, uid: Option<u32>, client: &[&str], server: &[&str]) {
    let case = format!("{receiver:?} uid={uid:?} client={client:?} server={server:?}");
    let (_temp, base) = fixture(&[("f0", 64), ("f1", 64)]);
    if plant(&base, uid).is_none() {
        return skip(&case);
    }
    let output = run(&mut command(receiver, &base, false, client, server));
    assert!(output.status.success(), "{case}: {}", describe(&output));
    assert_eq!(entries(&base.join("outside")), ["f0", "f1"], "{case}");
}

/// (a) A destination symlink owned by another uid is refused, absolute and
/// relative, by every receiver.
#[test]
fn an_untrusted_destination_symlink_is_refused_by_every_receiver() {
    for receiver in RECEIVERS {
        assert_refused(receiver, false);
        assert_refused(receiver, true);
    }
}

/// (b) The operator's own symlinked destination is followed.
#[test]
fn an_own_destination_symlink_is_followed_by_every_receiver() {
    for receiver in RECEIVERS {
        assert_followed(receiver, None, &[], &[]);
    }
}

/// (c) A root-owned symlinked destination (`/backup -> /mnt/disk`) is followed.
#[test]
fn a_root_owned_destination_symlink_is_followed_by_every_receiver() {
    for receiver in RECEIVERS {
        assert_followed(receiver, Some(0), &[], &[]);
    }
}

/// (d) `--insecure-links` on the receiving side restores the plain follow.
#[test]
fn insecure_links_follows_an_untrusted_destination_symlink() {
    assert_followed(
        Receiver::Local,
        Some(UNTRUSTED_UID),
        &["--insecure-links"],
        &[],
    );
    assert_followed(
        Receiver::Pull,
        Some(UNTRUSTED_UID),
        &["--insecure-links"],
        &[],
    );
    assert_followed(
        Receiver::Push,
        Some(UNTRUSTED_UID),
        &[],
        &["--insecure-links"],
    );
}

/// (e) A real destination directory transfers, absolute and relative.
#[test]
fn a_real_destination_directory_transfers_for_every_receiver() {
    for receiver in RECEIVERS {
        for relative in [false, true] {
            let (_temp, base) = fixture(&[("f0", 64)]);
            fs::create_dir(base.join("dest/sub")).expect("mkdir dest/sub");
            let output = run(&mut command(receiver, &base, relative, &[], &[]));
            assert!(
                output.status.success(),
                "{receiver:?}: {}",
                describe(&output)
            );
            assert_eq!(entries(&base.join("dest/sub")), ["f0"], "{receiver:?}");
        }
    }
}

/// The destination is swapped for a symlink to `outside` after the receiver
/// entered it, while the first file is still in flight. Every later write - the
/// rest of the first file, its commit, and the second file - must land in the
/// directory that was entered, as upstream's `fchdir()` guarantees, and nothing
/// may be created through the swapped-in symlink.
///
/// The exit status is deliberately not asserted. The network receiver still
/// applies some file metadata by path, and after the swap those stats fail
/// closed (`ENOENT`, exit 23) instead of reaching the entered directory; that is
/// the per-site resolver migration, not this anchor.
///
/// `--bwlimit` stretches both files. The swap waits for the first file to
/// appear - a network receiver's temp file as soon as it starts writing, a
/// local copy's `O_TMPFILE` only when it commits - which proves the receiver
/// is past its setup, and the second file is still to be written after the
/// swap whichever way the first one became visible.
fn assert_swap_after_entry_is_contained(receiver: Receiver) {
    const BIG: usize = 1 << 20;
    let (_temp, base) = fixture(&[("a_big", BIG), ("b_big", BIG)]);
    let sub = base.join("dest/sub");
    fs::create_dir(&sub).expect("mkdir dest/sub");
    let child = command(receiver, &base, false, &["--bwlimit=128"], &[])
        .spawn()
        .expect("spawn oc-rsync");
    let deadline = Instant::now() + RUN_TIMEOUT;
    while !entries(&sub).iter().any(|name| name.contains("a_big")) {
        assert!(
            Instant::now() < deadline,
            "{receiver:?}: the receiver never started writing a_big"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let entered = base.join("dest/entered");
    fs::rename(&sub, &entered).expect("move the entered destination aside");
    symlink(base.join("outside"), &sub).expect("swap in the symlink");
    let output = wait(child);
    assert!(
        entries(&base.join("outside")).is_empty(),
        "{receiver:?}: a write followed the swapped-in symlink: {:?}\n{}",
        entries(&base.join("outside")),
        describe(&output)
    );
    assert_eq!(
        entries(&entered),
        ["a_big", "b_big"],
        "{receiver:?}: {}",
        describe(&output)
    );
    for name in ["a_big", "b_big"] {
        assert_eq!(
            fs::read(entered.join(name))
                .expect("read landed file")
                .len(),
            BIG,
            "{receiver:?}: {name}"
        );
    }
}

#[test]
fn a_destination_swapped_after_entry_is_contained_local() {
    assert_swap_after_entry_is_contained(Receiver::Local);
}

#[test]
fn a_destination_swapped_after_entry_is_contained_push() {
    assert_swap_after_entry_is_contained(Receiver::Push);
}

#[test]
fn a_destination_swapped_after_entry_is_contained_pull() {
    assert_swap_after_entry_is_contained(Receiver::Pull);
}
