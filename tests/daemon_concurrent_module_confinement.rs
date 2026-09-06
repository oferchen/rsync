//! One module's `insecure links = yes` must not unconfine a CONCURRENT
//! connection to a module that never opted out.
//!
//! Upstream's `symlink_optout_allowed()` (syscall.c:122-127) answers
//! `module_id >= 0 && lp_insecure_links(module_id)` for a daemon - a property
//! of the SERVED MODULE. Upstream may keep that in a global because it forks a
//! child per connection (clientserver.c:1040-1122 all run in the child), so the
//! global is already private to one module.
//!
//! oc serves every daemon connection on a worker thread of ONE process
//! (`spawn_connection_worker`,
//! daemon/sections/server_runtime/connection.rs:198-229). A process-global
//! opt-out therefore answers whichever module published LAST. Measured before
//! the fix, against an rsync 3.5.0 client driving two simultaneous pulls: the
//! module that never opted out followed a symlink out of its own root in 47 of
//! 80 rounds, materialising a file from outside the module. That is a
//! confinement WIDENING driven entirely by an unrelated connection.
//!
//! The fix carries the opt-out as a per-connection value
//! (`ConnectionConfig::daemon_insecure_links`), read on the same axis as the
//! module root by `confinement_root()`
//! (transfer/src/generator/context.rs). This test is the guard on that: it is
//! the only thing standing between the value-threaded shape and a future
//! author "simplifying" it back into the global, which looks correct in every
//! serial test.
//!
//! # Non-vacuity
//!
//! [`loose_alone_still_opts_out`] is the positive control and it is load
//! bearing. If the opt-out were simply broken - confinement hard-on for every
//! module - the concurrency assertion below would pass for the wrong reason and
//! this file would be inert. That test fails if the feature stops working, so
//! the pair can only be green when the opt-out works AND stays scoped to its
//! own connection.
//!
//! Skip condition (test passes with a printed reason): loopback TCP is
//! unavailable, the platform cannot create symlinks, or the daemon does not
//! answer.

#![cfg(unix)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The file that lives OUTSIDE both module roots. Its arrival under a
/// destination is the escape.
const SECRET: &str = "secret.txt";
const SECRET_BODY: &[u8] = b"outside both modules\n";

/// Concurrent rounds. The pre-fix leak rate was ~59% per round, so this is far
/// past the point where a surviving race would go unnoticed; it stays small
/// enough to keep the test quick.
const ROUNDS: usize = 24;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
}

/// Builds the fixture: two modules, each holding a symlink that escapes its own
/// root into a shared outside directory.
///
/// Both modules carry the escape so the two differ ONLY in the `insecure links`
/// setting - if the escape were present in just one, a difference in outcome
/// could be attributed to the fixture rather than to the opt-out.
fn build_fixture(root: &Path) -> std::io::Result<(PathBuf, PathBuf)> {
    let outside = root.join("outside");
    fs::create_dir_all(&outside)?;
    fs::write(outside.join(SECRET), SECRET_BODY)?;

    let strict = root.join("strict");
    let loose = root.join("loose");
    for module in [&strict, &loose] {
        fs::create_dir_all(module.join("sub"))?;
        for i in 0..8 {
            fs::write(module.join("sub").join(format!("f{i}")), b"in module\n")?;
        }
        std::os::unix::fs::symlink(&outside, module.join("escape"))?;
    }
    Ok((strict, loose))
}

fn write_config(conf: &Path, port: u16, strict: &Path, loose: &Path) -> std::io::Result<()> {
    fs::write(
        conf,
        format!(
            "port = {port}\n\
             \n\
             [strict]\n\
             \tpath = {strict}\n\
             \tread only = yes\n\
             \tuse chroot = no\n\
             \n\
             [loose]\n\
             \tpath = {loose}\n\
             \tread only = yes\n\
             \tuse chroot = no\n\
             \tinsecure links = yes\n",
            strict = strict.display(),
            loose = loose.display(),
        ),
    )
}

fn spawn_daemon(conf: &Path, port: u16) -> Option<Child> {
    let mut child = Command::new(oc_binary())
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", conf.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Some(child);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

/// Pulls a whole module with `-L`, so a symlink is resolved by the SENDER and
/// the escape decision is the daemon's, not the client's.
fn pull(port: u16, module: &str, dest: &Path) {
    let _ = Command::new(oc_binary())
        .arg("-r")
        .arg("-L")
        .arg("--timeout=20")
        .arg(format!("rsync://127.0.0.1:{port}/{module}/"))
        .arg(format!("{}/", dest.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Whether the outside file arrived through the module's escaping symlink.
fn escaped(dest: &Path) -> bool {
    dest.join("escape").join(SECRET).exists()
}

/// Reaps the daemon even if an assertion below panics.
struct Reaper(Child);
impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    root: PathBuf,
    port: u16,
    _daemon: Reaper,
}

/// Returns `None` when the environment cannot host the test.
fn setup() -> Option<Fixture> {
    let port = free_port()?;
    let tmp = tempfile::tempdir().ok()?;
    let root = tmp.path().to_path_buf();
    let (strict, loose) = build_fixture(&root).ok()?;
    let conf = root.join("rsyncd.conf");
    write_config(&conf, port, &strict, &loose).ok()?;
    let daemon = spawn_daemon(&conf, port)?;
    Some(Fixture {
        _root: tmp,
        root,
        port,
        _daemon: Reaper(daemon),
    })
}

/// Positive control. Without this, a build that confined every module
/// unconditionally would satisfy the concurrency test vacuously.
#[test]
fn loose_alone_still_opts_out() {
    let Some(fx) = setup() else {
        println!("skipping: loopback TCP unavailable or daemon did not answer");
        return;
    };
    let dest = fx.root.join("ctl_loose");
    fs::create_dir_all(&dest).expect("dest");
    pull(fx.port, "loose", &dest);
    assert!(
        escaped(&dest),
        "`insecure links = yes` no longer opts out of the symlink confinement, \
         so the concurrency assertion in this file proves nothing"
    );
}

/// Negative control: the module that never opted out is confined when it is the
/// only connection. Stays green under every mutation of the per-connection
/// threading, because serially the global and the value always agree.
#[test]
fn strict_alone_is_confined() {
    let Some(fx) = setup() else {
        println!("skipping: loopback TCP unavailable or daemon did not answer");
        return;
    };
    let dest = fx.root.join("ctl_strict");
    fs::create_dir_all(&dest).expect("dest");
    pull(fx.port, "strict", &dest);
    assert!(
        !escaped(&dest),
        "a module without `insecure links` followed a symlink out of its own root \
         even with no other connection in flight"
    );
}

/// The defect: two simultaneous connections on DIFFERENT modules, where one has
/// opted out and the other has not. Neither may observe the other's boundary.
#[test]
fn a_concurrent_optout_module_does_not_unconfine_a_strict_one() {
    let Some(fx) = setup() else {
        println!("skipping: loopback TCP unavailable or daemon did not answer");
        return;
    };

    let mut leaked_rounds = Vec::new();
    for round in 0..ROUNDS {
        let strict_dest = fx.root.join(format!("s{round}"));
        let loose_dest = fx.root.join(format!("l{round}"));
        fs::create_dir_all(&strict_dest).expect("dest");
        fs::create_dir_all(&loose_dest).expect("dest");

        let port = fx.port;
        // Both pulls must be in flight at once: the leak is one connection
        // publishing its opt-out into state another connection reads.
        std::thread::scope(|scope| {
            let sd = &strict_dest;
            let ld = &loose_dest;
            scope.spawn(move || pull(port, "strict", sd));
            scope.spawn(move || pull(port, "loose", ld));
        });

        if escaped(&strict_dest) {
            leaked_rounds.push(round);
        }
    }

    assert!(
        leaked_rounds.is_empty(),
        "the strict module escaped its own root in {} of {ROUNDS} concurrent rounds \
         (rounds {leaked_rounds:?}) - a connection to `loose` switched off the \
         confinement of a connection to `strict`",
        leaked_rounds.len(),
    );
}
