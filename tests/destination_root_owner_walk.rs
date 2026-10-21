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

/// A kernel notification on changes to one directory's entry list, so a test
/// can act on the receiver's own write instead of sleep-polling for it.
struct DirWatch {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    inotify: nix::sys::inotify::Inotify,
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    kqueue: nix::sys::event::Kqueue,
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    _dir: fs::File,
}
impl DirWatch {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn new(dir: &Path) -> Self {
        use nix::sys::inotify::{AddWatchFlags, InitFlags, Inotify};
        let inotify =
            Inotify::init(InitFlags::IN_CLOEXEC | InitFlags::IN_NONBLOCK).expect("inotify_init");
        inotify
            .add_watch(dir, AddWatchFlags::IN_CREATE | AddWatchFlags::IN_MOVED_TO)
            .expect("inotify_add_watch");
        Self { inotify }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn new(dir: &Path) -> Self {
        use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent, Kqueue};
        use std::os::fd::AsRawFd;
        let dir_file = fs::File::open(dir).expect("open the watched directory");
        let kqueue = Kqueue::new().expect("kqueue");
        let change = KEvent::new(
            usize::try_from(dir_file.as_raw_fd()).expect("fd"),
            EventFilter::EVFILT_VNODE,
            EvFlags::EV_ADD | EvFlags::EV_CLEAR,
            FilterFlag::NOTE_WRITE,
            0,
            0,
        );
        kqueue
            .kevent(&[change], &mut [], None)
            .expect("register the watch");
        Self {
            kqueue,
            _dir: dir_file,
        }
    }
    /// Blocks for at most `budget` until the directory changes.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn wait(&self, budget: Duration) {
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
        use std::os::fd::AsFd;
        let mut fds = [PollFd::new(self.inotify.as_fd(), PollFlags::POLLIN)];
        poll(
            &mut fds,
            PollTimeout::try_from(budget).unwrap_or(PollTimeout::MAX),
        )
        .expect("poll the watch");
        // Non-blocking: an empty queue after a timeout reads as EAGAIN.
        let _ = self.inotify.read_events();
    }
    /// Blocks for at most `budget` until the directory changes.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn wait(&self, budget: Duration) {
        use nix::sys::event::{EvFlags, EventFilter, FilterFlag, KEvent};
        let mut events = [KEvent::new(
            0,
            EventFilter::EVFILT_VNODE,
            EvFlags::empty(),
            FilterFlag::empty(),
            0,
            0,
        )];
        let timeout = libc::timespec {
            tv_sec: libc::time_t::try_from(budget.as_secs()).unwrap_or(libc::time_t::MAX),
            tv_nsec: libc::c_long::from(i32::try_from(budget.subsec_nanos()).expect("nanos")),
        };
        self.kqueue
            .kevent(&[], &mut events, Some(timeout))
            .expect("wait on the watch");
    }
    /// Blocks until an entry whose name contains `needle` exists in `dir`.
    ///
    /// The check runs before every wait, so an entry that arrived before the
    /// watch fired - or before it was armed - is still seen.
    fn await_entry(&self, dir: &Path, needle: &str, budget: Duration) {
        let deadline = Instant::now() + budget;
        while !entries(dir).iter().any(|name| name.contains(needle)) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "nothing named {needle} appeared in {}",
                dir.display()
            );
            self.wait(remaining);
        }
    }
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
/// entered it and committed its first file. Every later operation - the second
/// and third files' temp-file creates, their commits and attribute changes, and
/// the closing directory touch-ups - must land in the directory that was
/// entered, as upstream's `fchdir()` guarantees: the run exits 0, all three
/// files are in the entered directory, and `outside` is untouched.
///
/// `outside` is read-only, so a create resolved through the swapped-in symlink
/// fails instead of silently landing elsewhere, and its mode and mtime are
/// compared so a directory step that followed the symlink is caught too.
///
/// The swap is driven by a kernel directory notification on the first file's
/// arrival, not by polling. `--bwlimit` stretches each file to about two
/// seconds, so the third file is wholly written after the swap whichever way
/// the first one became visible (a network receiver's temp file, or a local
/// copy's `O_TMPFILE` link at commit).
///
/// upstream: `rsync-3.5.1/main.c:865` and `rsync-3.5.1/util1.c:1448-1466`
/// (enter with `fchdir`), `rsync-3.5.1/receiver.c:413` (relative temp names).
fn assert_swap_after_entry_is_contained(receiver: Receiver) {
    const LEN: usize = 256 << 10;
    const FILES: [&str; 3] = ["a_big", "b_big", "c_big"];
    let (_temp, base) = fixture(&FILES.map(|name| (name, LEN)));
    let sub = base.join("dest/sub");
    fs::create_dir(&sub).expect("mkdir dest/sub");
    let outside = base.join("outside");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o555)).expect("chmod outside");
    let outside_before = fs::metadata(&outside).expect("stat outside");
    let entered = base.join("dest/entered");
    let watch = DirWatch::new(&sub);
    let swapper = {
        let (sub, entered, outside) = (sub.clone(), entered.clone(), outside.clone());
        std::thread::spawn(move || {
            watch.await_entry(&sub, "a_big", RUN_TIMEOUT);
            fs::rename(&sub, &entered).expect("move the entered destination aside");
            symlink(&outside, &sub).expect("swap in the symlink");
        })
    };
    let mut cmd = command(receiver, &base, false, &["--bwlimit=128"], &[]);
    let output = match test_support::deadline::run_deadlined(&mut cmd, RUN_TIMEOUT)
        .expect("spawn oc-rsync")
    {
        test_support::deadline::Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => Output {
            status,
            stdout,
            stderr,
        },
        expired => panic!("{receiver:?}: oc-rsync did not finish: {expired:?}"),
    };
    swapper.join().expect("the swap ran");
    assert!(
        output.status.success(),
        "{receiver:?}: {}",
        describe(&output)
    );
    assert!(
        entries(&outside).is_empty(),
        "{receiver:?}: a write followed the swapped-in symlink: {:?}\n{}",
        entries(&outside),
        describe(&output)
    );
    let outside_after = fs::metadata(&outside).expect("stat outside");
    assert_eq!(
        (
            outside_after.mode(),
            outside_after.mtime(),
            outside_after.mtime_nsec()
        ),
        (
            outside_before.mode(),
            outside_before.mtime(),
            outside_before.mtime_nsec()
        ),
        "{receiver:?}: a directory step followed the swapped-in symlink"
    );
    assert_eq!(
        entries(&entered),
        FILES,
        "{receiver:?}: {}",
        describe(&output)
    );
    for name in FILES {
        assert_eq!(
            fs::read(entered.join(name))
                .expect("read landed file")
                .len(),
            LEN,
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
