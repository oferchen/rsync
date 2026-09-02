//! A chrooted module must still resolve the `nobody` uid/gid defaults.
//!
//! Upstream resolves the identity to drop to BEFORE it enters the jail:
//!
//! ```c
//! /* clientserver.c:831-843 - rsync_module() */
//! am_root = (uid == ROOT_UID);
//! p = *lp_uid(module_id) ? lp_uid(module_id) : am_root ? NOBODY_USER : NULL;
//! if (p) {
//!         if (!user_to_uid(p, &uid, True)) {
//!                 rprintf(FLOG, "Invalid uid %s\n", p);
//!                 io_printf(f_out, "@ERROR: invalid uid %s\n", p);
//! ...
//! /* clientserver.c:873 - the NOBODY_GROUP default, same pre-chroot region */
//!         if (add_a_group(f_out, NOBODY_GROUP) < 0)
//! ...
//! /* clientserver.c:1040-1050 - only now does the jail exist */
//! if (use_chroot) {
//!         ...
//!         if (chroot(module_chdir)) {
//! ```
//!
//! The order is load-bearing, not incidental. `user_to_uid` / `group_to_gid`
//! are NSS lookups: they read `/etc/passwd`, `/etc/group` and the NSS shared
//! objects. A module root is not a system root, so inside the jail none of
//! those exist and the lookup cannot succeed. Resolving after the chroot turns
//! the `nobody` default that a root daemon applies to EVERY module without an
//! explicit numeric `uid` into `@ERROR: invalid uid nobody`, and the module
//! serves nothing at all.
//!
//! Only the `setgid`/`setgroups`/`setuid` syscalls belong after the chroot -
//! they and the chroot both need the root privileges being dropped.
//!
//! WHY THIS TEST NEEDS ROOT, AND WHY THE SKIP IS NOT A PASS IN DISGUISE.
//!
//! The defect needs three things at once: `am_root` (only a root daemon
//! applies the `nobody` default), an effective `use chroot`, and a uid/gid left
//! to that default. A non-root daemon reaches neither the default nor
//! `chroot(2)`, so the cell is structurally unable to fail there and prints a
//! reason instead of asserting. CI's `nextest` leg runs unprivileged, so this
//! cell reports SKIP there; it is written to fire for anyone running the suite
//! as root, which is the only configuration that can observe the behaviour.
//!
//! Skip conditions (the test prints a reason and returns):
//! - not running as root;
//! - no `nobody` account on the host (the lookup would fail for a real reason);
//! - loopback TCP unavailable;
//! - `chroot(2)` refused by the environment (a rootless container maps uid 0
//!   without granting CAP_SYS_CHROOT).
#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

fn running_as_root() -> bool {
    id_of(&["-u"]).as_deref() == Some("0")
}

/// The uid the daemon will drop to, or `None` when the host has no such
/// account. Reported as a skip rather than asserted: without `nobody` the
/// lookup fails for a reason that has nothing to do with the chroot ordering.
fn nobody_uid() -> Option<String> {
    id_of(&["-u", "nobody"])
}

fn id_of(args: &[&str]) -> Option<String> {
    let out = Command::new("id").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if value.is_empty() { None } else { Some(value) }
}

/// Starts a daemon and waits until its port answers.
///
/// `stdin` is `/dev/null` deliberately: with an inherited terminal or pipe the
/// daemon takes its single-connection inetd path and never listens, and the
/// client then reports "connection refused" instead of the behaviour under
/// test.
fn spawn_daemon(binary: &Path, conf: &Path, port: u16) -> Child {
    let child = Command::new(binary)
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

struct Outcome {
    transferred: bool,
    landed: bool,
    log: String,
}

/// Pushes one file into a `use chroot = yes` module and reports what happened.
///
/// `identity` is written verbatim into the module section, so the caller
/// chooses between the `nobody` default (empty) and explicit numeric ids.
fn push_into_chrooted_module(identity: &str, drop_uid: &str) -> Option<Outcome> {
    let root = tempfile::tempdir().expect("temp dir");
    let port = free_port()?;

    let module = root.path().join("mod");
    let src = root.path().join("src");
    fs::create_dir_all(&module).expect("module dir");
    fs::create_dir_all(&src).expect("source dir");
    fs::write(src.join("f.txt"), b"payload\n").expect("source file");

    // The daemon drops to the unprivileged identity before it writes, so the
    // module root must be writable by that uid. Without this the receiver
    // fails on a plain permission error and the cell would report the same
    // "nothing landed" as the defect while proving nothing about it.
    let chowned = Command::new("chown")
        .arg("-R")
        .arg(format!("{drop_uid}:{drop_uid}"))
        .arg(&module)
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !chowned {
        return None;
    }

    let log_path = root.path().join("daemon.log");
    let conf = format!(
        "port = {port}\n\
         use chroot = yes\n\
         log file = {log}\n\
         \n\
         [m]\n\
         \tpath = {module}\n\
         \tread only = no\n\
         {identity}",
        log = log_path.display(),
        module = module.display(),
    );
    let conf_path = root.path().join("rsyncd.conf");
    fs::write(&conf_path, conf).expect("config");

    let binary = oc_binary();
    let mut daemon = spawn_daemon(&binary, &conf_path, port);
    let status = Command::new(&binary)
        .args([
            "-q",
            "-r",
            &format!("{}/", src.display()),
            &format!("rsync://127.0.0.1:{port}/m/"),
        ])
        .status()
        .expect("run client");

    let landed = module.join("f.txt").exists();
    let _ = daemon.kill();
    let _ = daemon.wait();

    Some(Outcome {
        transferred: status.success(),
        landed,
        log: fs::read_to_string(&log_path).unwrap_or_default(),
    })
}

/// The module leaves `uid`/`gid` unset, so a root daemon applies the `nobody`
/// defaults - the commonest chrooted configuration there is.
#[test]
fn chrooted_module_serves_under_the_nobody_default() {
    if !running_as_root() {
        println!("SKIP: needs root - a non-root daemon applies no nobody default");
        return;
    }
    let Some(drop_uid) = nobody_uid() else {
        println!("SKIP: host has no `nobody` account");
        return;
    };
    let Some(outcome) = push_into_chrooted_module("", &drop_uid) else {
        println!("SKIP: no loopback port, or the module could not be chowned");
        return;
    };
    if outcome.log.contains("chroot") && outcome.log.contains("not permitted") {
        println!("SKIP: chroot(2) refused by this environment");
        return;
    }

    // Non-vacuity for the PRECONDITION: a chroot that silently did not happen
    // would make every assertion below pass while testing nothing, because the
    // lookup would then run in the ordinary root filesystem either way.
    assert!(
        outcome.log.contains("chroot applied"),
        "the daemon must actually have chrooted, otherwise this cell is inert; log:\n{}",
        outcome.log
    );
    assert!(
        !outcome.log.contains("Invalid uid") && !outcome.log.contains("Invalid gid"),
        "the `nobody` uid/gid must resolve before the chroot \
         (clientserver.c:831-873 precede the chroot at :1040-1050); log:\n{}",
        outcome.log
    );
    assert!(outcome.transferred, "the push must succeed");
    assert!(
        outcome.landed,
        "the pushed file must reach the module - an exit code alone does not \
         show that the module served anything"
    );
}

/// The non-vacuity companion: fully numeric ids reach ZERO name lookups, so
/// this cell is green whichever side of the chroot the resolution happens on.
/// Without it, a change that broke chrooted modules outright would look like a
/// fixture problem rather than a regression in the identity resolution.
#[test]
fn chrooted_module_with_numeric_ids_is_unaffected() {
    if !running_as_root() {
        println!("SKIP: needs root - a non-root daemon cannot set uid/gid");
        return;
    }
    let Some(drop_uid) = nobody_uid() else {
        println!("SKIP: host has no `nobody` account");
        return;
    };
    let identity = format!("\tuid = {drop_uid}\n\tgid = {drop_uid}\n");
    let Some(outcome) = push_into_chrooted_module(&identity, &drop_uid) else {
        println!("SKIP: no loopback port, or the module could not be chowned");
        return;
    };
    if outcome.log.contains("chroot") && outcome.log.contains("not permitted") {
        println!("SKIP: chroot(2) refused by this environment");
        return;
    }

    assert!(
        outcome.log.contains("chroot applied"),
        "the daemon must actually have chrooted; log:\n{}",
        outcome.log
    );
    assert!(outcome.transferred, "the push must succeed");
    assert!(outcome.landed, "the pushed file must reach the module");
}
