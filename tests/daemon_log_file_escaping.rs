//! Peer-supplied text reaching the daemon's log file is escaped (CWE-117).
//!
//! Upstream escapes in the *sink*, not in any renderer: `logit()` is the only
//! function that writes `logfile_fp`, and it hands every line to
//! `filtered_fwrite(logfile_fp, buf, len, 0, 1, trailing)` (log.c:126-132).
//! Nothing that reaches the log file can skip it, so no call site needs to
//! pre-clean its operands - and none of upstream's do. oc-rsync ports that
//! `filtered_fwrite` faithfully in `crates/logging-sink/src/escape.rs`.
//!
//! The vector is the **requested module name**. The peer transmits it on the
//! `@RSYNCD` wire and the daemon writes it to the module's log file at two
//! sites, both mirroring `clientserver.c:787`'s
//! `rprintf(FLOG, "rsync %s %s from %s@%s (%s)\n", ...)`.
//!
//! ⚠ This test previously used the client *argument vector* as its vector.
//! PR #7633 stopped logging that vector verbatim - deliberately, and correctly:
//! upstream never writes it to the daemon log at all, and echoing it made a
//! connection a log-amplification primitive. That removal left this test with
//! no observable subject, which is what its own non-vacuity guard caught. The
//! module name is a vector upstream does keep, so the subject is re-pinned
//! there rather than abandoned.
//!
//! ⚠ Only `ESC` is exercised, not `LF`. The module name is a *line-terminated*
//! field on the `@RSYNCD` wire, so an embedded `LF` cannot traverse it by
//! construction - asserting on one would pin a byte this vector can never
//! deliver. `escape.rs`'s own unit tests cover the `\#012` rendering.
//!
//! ⚠ Deliberately NOT covered here: an *unresolvable* module name. Upstream
//! logs those too (`clientserver.c:1568`, "unknown module '%s' tried from ..."),
//! but oc opens a log sink only per resolved module, so nothing before module
//! resolution reaches a log file at all. That gap is real and separate; it
//! cannot be asserted from this file without first opening a session-level
//! sink, and pretending otherwise here would only make the test unrunnable.
//!
//! Skip condition (test passes with a printed reason): loopback TCP is
//! unavailable, or the daemon does not answer.

#![cfg(unix)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A module name carrying `ESC`, the byte that starts a terminal control
/// sequence and so forges log output if it lands raw.
const HOSTILE_MODULE: &str = "A\x1bB";

/// The same name as the log file must render it: 0x1B is octal 033, and every
/// other byte is ASCII-printable and survives verbatim.
const ESCAPED_MODULE: &str = "A\\#033B";

/// The rendering a *pre-cleaning* call site produces instead: control bytes
/// collapsed to `?`. That is lossy and irreversible, and it is what this test
/// exists to keep out of the log.
const MANGLED_MODULE: &str = "A?B";

/// A wholly printable module name, used as the run's non-vacuity control: it
/// must reach the log unchanged, so a sink that mangled or dropped every
/// requested name could not satisfy both assertions.
const PLAIN_MODULE: &str = "plain-name";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Reserves a loopback port by binding and immediately dropping the listener.
///
/// Returns `None` when loopback TCP is unavailable, which is the sandbox
/// condition this test skips on rather than fails on.
fn reserve_port() -> Option<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Writes the daemon config: a global `log file` plus the two modules whose
/// names this test requests.
fn write_config(dir: &Path, log: &Path, module_root: &Path, port: u16) -> PathBuf {
    let path = dir.join("rsyncd.conf");
    let body = format!(
        "port = {port}\n\
         use chroot = no\n\
         log file = {log}\n\
         \n\
         [{HOSTILE_MODULE}]\n\
         \tpath = {root}\n\
         \tread only = yes\n\
         \n\
         [{PLAIN_MODULE}]\n\
         \tpath = {root}\n\
         \tread only = yes\n",
        log = log.display(),
        root = module_root.display(),
    );
    fs::write(&path, body).expect("write daemon config");
    path
}

/// Polls the daemon's port until it answers, so the test never races startup.
fn wait_until_listening(port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Requests one module over a raw `@RSYNCD` handshake.
///
/// The bytes are written directly rather than through the client so the module
/// name reaches the daemon exactly as typed - a client-side operand parser
/// could launder the very byte under test.
fn request_module(port: u16, module: &str) -> std::io::Result<()> {
    let stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut greeting = String::new();
    reader.read_line(&mut greeting)?;

    let mut writer = stream;
    writer.write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")?;
    writer.write_all(module.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;

    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    Ok(())
}

/// Reaps the daemon whatever the test's outcome, so a failed assertion cannot
/// leave a listener behind.
struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn daemon_log_file_escapes_a_peer_requested_module_name() {
    let temp = tempfile::tempdir().expect("temp dir");
    let root = temp.path();
    let log = root.join("daemon.log");
    let module_root = root.join("module");
    fs::create_dir(&module_root).expect("module root");

    let Some(port) = reserve_port() else {
        eprintln!("skipping: loopback TCP unavailable");
        return;
    };
    let config = write_config(root, &log, &module_root, port);

    let child = Command::new(oc_binary())
        .arg("--daemon")
        .arg("--no-detach")
        .arg(format!("--config={}", config.display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");
    let _guard = DaemonGuard(child);

    if !wait_until_listening(port) {
        eprintln!("skipping: daemon did not answer on 127.0.0.1:{port}");
        return;
    }

    request_module(port, HOSTILE_MODULE).expect("request the hostile module name");
    request_module(port, PLAIN_MODULE).expect("request the control module name");

    let contents = fs::read(&log).expect("read daemon log");
    let rendered = String::from_utf8_lossy(&contents).into_owned();

    // Non-vacuity: without the control module in the log, every assertion below
    // would be satisfiable by a daemon that simply logged nothing.
    assert!(
        rendered.contains(PLAIN_MODULE),
        "the control module never reached the log, so the assertions below would be vacuous:\n{rendered}"
    );

    // The subject: the ESC-bearing name is present, reversibly escaped.
    assert!(
        rendered.contains(ESCAPED_MODULE),
        "the hostile module name must appear escaped as {ESCAPED_MODULE:?}:\n{rendered}"
    );

    // Both sites that name the module must agree. `rsync allowed access on
    // module ...` (clientserver.c:787) and the client-args line each render the
    // same peer string; a call site that pre-cleans its operand renders the
    // mangled form instead, which is lossy and cannot be undone by a reader.
    assert!(
        !rendered.contains(MANGLED_MODULE),
        "no log line may pre-mangle the module name to {MANGLED_MODULE:?}; \
         escaping belongs to the sink:\n{rendered}"
    );

    // The point of escaping: the raw control byte never reaches the file.
    assert!(
        !contents.contains(&0x1b),
        "a raw ESC byte reached the log file:\n{rendered}"
    );
}
