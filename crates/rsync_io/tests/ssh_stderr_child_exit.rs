//! The remote shell's stderr is inherited, never captured or re-printed.
//!
//! upstream: pipe.c:48-97 `piped_child()` redirects only stdin and stdout, so
//! the child writes its diagnostics straight to rsync's own fd 2: byte for
//! byte, as soon as they are written, and exactly once. Each test re-executes
//! this test binary as the "client" so the assertions run against that
//! process's real stderr, which is what a user sees.

#![cfg(unix)]

use std::ffi::OsString;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rsync_io::ssh::SshCommand;

/// Names the shell script the re-executed client should run as its remote shell.
const CHILD_SCRIPT_ENV: &str = "OC_RSYNC_TEST_STDERR_CHILD_SCRIPT";
/// When set, the client drops the connection instead of waiting on it.
const CHILD_DROP_ENV: &str = "OC_RSYNC_TEST_STDERR_CHILD_DROP";

/// How the re-executed client tears down the remote shell.
#[derive(Clone, Copy)]
enum Teardown {
    /// Split the connection and reap the child with `SshChildHandle::wait`.
    Wait,
    /// Let the child exit, then drop the connection without waiting, as an
    /// early-return error path does.
    Drop,
}

/// Client side: spawn `/bin/sh -c <script>` through [`SshCommand`] and tear it
/// down as the transfer path does. Inert unless re-executed by a test below.
#[test]
fn stderr_inherit_child_entry() {
    let Some(script) = std::env::var_os(CHILD_SCRIPT_ENV) else {
        return;
    };
    let mut command = SshCommand::new("ignored");
    command.set_program("/bin/sh");
    command.push_option("-c");
    command.push_option(script);
    command.set_target_override(Some(OsString::new()));
    let mut connection = command.spawn().expect("spawn /bin/sh");
    if std::env::var_os(CHILD_DROP_ENV).is_some() {
        while connection.try_wait().expect("poll child").is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(connection);
        std::process::exit(0);
    }
    let (_reader, _writer, child) = connection.split().expect("split connection");
    let status = child.wait().expect("reap child");
    std::process::exit(status.code().unwrap_or(1));
}

/// Re-executes this test binary as the client running `script`, with stderr piped.
fn spawn_client(script: &str, teardown: Teardown) -> std::process::Child {
    let mut client = Command::new(std::env::current_exe().expect("test binary path"));
    client
        .args([
            "--exact",
            "stderr_inherit_child_entry",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_SCRIPT_ENV, script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if matches!(teardown, Teardown::Drop) {
        client.env(CHILD_DROP_ENV, "1");
    }
    client.spawn().expect("re-exec test binary")
}

/// Bytes the remote shell writes reach the client's stderr unchanged and
/// immediately: a non-UTF-8 sequence and a fragment with no trailing newline
/// must be readable while the shell is still running, not held until exit.
#[test]
fn remote_shell_stderr_reaches_the_user_verbatim_and_unbuffered() {
    let marker: &[u8] = b"prompt:\xff\xfe-no-newline";
    let mut client = spawn_client(
        "printf 'prompt:\\377\\376-no-newline' >&2; sleep 5",
        Teardown::Wait,
    );
    let mut stderr = client.stderr.take().expect("client stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&buf[..n]);
            if seen.windows(marker.len()).any(|w| w == marker) {
                let _ = tx.send(seen.clone());
            }
        }
    });
    let started = Instant::now();
    let seen = rx
        .recv_timeout(Duration::from_secs(3))
        .expect("the no-newline, non-UTF-8 fragment must reach stderr before the shell exits");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "fragment arrived only after {:?}",
        started.elapsed()
    );
    assert!(
        seen.windows(marker.len()).any(|w| w == marker),
        "bytes must arrive unmodified, got {seen:?}"
    );
    let _ = client.kill();
    let _ = client.wait();
}

/// The diagnostic line a failing remote shell writes before exiting 255.
const REFUSED: &str = "ssh: connect to host example port 22: Connection refused";

/// Counts how often a failing remote shell's diagnostic reaches the client's stderr.
fn refused_line_count(teardown: Teardown) -> (Option<i32>, usize, String) {
    let client = spawn_client(&format!("echo '{REFUSED}' >&2; exit 255"), teardown);
    let output = client.wait_with_output().expect("collect client output");
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    (output.status.code(), text.matches(REFUSED).count(), text)
}

/// A failing remote shell's diagnostic appears exactly once, and its exit
/// status is preserved when the child is reaped.
#[test]
fn failing_remote_shell_diagnostic_appears_exactly_once() {
    let (code, count, text) = refused_line_count(Teardown::Wait);
    assert_eq!(
        code,
        Some(255),
        "the remote shell's exit status must reach the client"
    );
    assert_eq!(
        count, 1,
        "the diagnostic must be printed exactly once, got: {text:?}"
    );
}

/// Dropping the connection without waiting (an early-return error path) must
/// not print the diagnostic a second time.
#[test]
fn dropped_connection_does_not_reprint_the_diagnostic() {
    let (_, count, text) = refused_line_count(Teardown::Drop);
    assert_eq!(
        count, 1,
        "the diagnostic must be printed exactly once, got: {text:?}"
    );
}
