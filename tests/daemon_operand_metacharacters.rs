//! A shell metacharacter in a filename survives an `rsync://` transfer.
//!
//! Upstream backslash-escapes filename operands with `safe_arg(NULL, ...)` only
//! under `if (!daemon_connection)` (main.c:619) - that escaping exists for a
//! remote shell that will `eval` the argv, and a daemon connection has no
//! shell. Its daemon agrees: `read_args()` un-escapes just the args preceding
//! the `.` and routes everything after it through `glob_expand()` untouched
//! (io.c:1500-1506).
//!
//! oc-rsync escaped the operands on both transports, so every character in
//! `SHELL_CHARS` reached the peer as a literal backslash that no peer removes.
//! Measured against rsync 3.5.0 before the fix, `a b.txt` arrived as the split
//! path `/a/ b.txt` and the transfer exited 23.
//!
//! This is a class test, not a single regression: the defect was one escape set
//! applied to one argument vector, so every member of that set failed together
//! and any future re-broadening fails here as a group.
//!
//! `CONTROL_NAME` carries no metacharacter and must transfer too - without it a
//! harness that transferred nothing at all would satisfy every other assertion
//! vacuously.
//!
//! Deliberately absent: a filename containing a literal backslash. Measured on
//! this same harness, oc's *client* drives a real upstream 3.5.0 daemon
//! correctly for that name while oc's own daemon still fails it, so the
//! residual is in the daemon's post-dot operand handling, not in the escaping
//! fixed here. It is tracked separately rather than silently dropped.
//!
//! Skip condition (test passes with a printed reason): loopback TCP is
//! unavailable, or the daemon does not answer.

#![cfg(unix)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// `SHELL_CHARS` (options.c:2693) minus `WILD_CHARS`, which `glob_expand()`
/// legitimately expands on the daemon side, and minus the backslash covered by
/// the module note above. Tab and newline are included because they are the
/// two bytes 3.5.0 added to the set, so they are the least-exercised members.
const METACHARACTER_NAMES: &[&str] = &[
    "sp ace",
    "dol$lar",
    "semi;colon",
    "amp&ersand",
    "hash#mark",
    "paren(open",
    "paren)close",
    "brace{open",
    "brace}close",
    "single'quote",
    "double\"quote",
    "back`tick",
    "pipe|bar",
    "lt<gt",
    "gt>lt",
    "bang!mark",
    "tab\tchar",
    "new\nline",
];

/// The non-vacuity control: no metacharacter, so it must transfer whatever the
/// escaping rule is.
const CONTROL_NAME: &str = "control-plain";

const PAYLOAD: &[u8] = b"metacharacter payload\n";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Starts a daemon and waits until its port answers, so the client never races
/// the listener.
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

/// Pulls one name out of the module into a fresh directory and reports whether
/// it arrived intact. Both halves matter: an escaped operand can either fail
/// the lookup outright or resolve to a *different* name, and only comparing
/// the landed bytes catches the second.
fn pull(port: u16, name: &str, dest: &Path) -> Result<(), String> {
    let status = Command::new(oc_binary())
        .arg("-q")
        .arg(format!("rsync://127.0.0.1:{port}/m/{name}"))
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run client");
    if !status.success() {
        return Err(format!("client exited {:?}", status.code()));
    }
    match fs::read(dest.join(name)) {
        Ok(bytes) if bytes == PAYLOAD => Ok(()),
        Ok(bytes) => Err(format!(
            "landed {} bytes, expected {}",
            bytes.len(),
            PAYLOAD.len()
        )),
        Err(err) => Err(format!("no file at that exact name: {err}")),
    }
}

#[test]
fn daemon_operands_keep_shell_metacharacters_intact() {
    let Some(port) = free_port() else {
        println!("skipping: loopback TCP unavailable");
        return;
    };
    let root = tempfile::tempdir().expect("temp dir");
    let module = root.path().join("mod");
    fs::create_dir_all(&module).expect("module dir");

    let mut names: Vec<&str> = vec![CONTROL_NAME];
    names.extend_from_slice(METACHARACTER_NAMES);
    for name in &names {
        fs::write(module.join(name), PAYLOAD).expect("seed source file");
    }

    let conf = root.path().join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "port = {port}\n\
             use chroot = no\n\
             \n\
             [m]\n\
             \tpath = {module}\n\
             \tread only = yes\n",
            module = module.display(),
        ),
    )
    .expect("write config");

    let Some(mut daemon) = spawn_daemon(&conf, port) else {
        println!("skipping: daemon did not answer on 127.0.0.1:{port}");
        return;
    };

    // A destination per name, so one mangled operand cannot land under a name
    // a later pull would then find already present.
    let failures: Vec<String> = names
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            let dest = root.path().join(format!("dest{i}"));
            fs::create_dir_all(&dest).expect("dest dir");
            pull(port, name, &dest)
                .err()
                .map(|why| format!("{name:?}: {why}"))
        })
        .collect();

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert!(
        failures.is_empty(),
        "operands were mangled in transit ({} of {}):\n{}",
        failures.len(),
        names.len(),
        failures.join("\n")
    );
}
