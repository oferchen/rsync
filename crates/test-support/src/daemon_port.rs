//! Race-free free-port allocation for daemon integration tests.
//!
//! # Why this exists
//!
//! Allocating an ephemeral port (bind port 0, read its number, drop the
//! listener) and then starting a separate daemon process on that number has a
//! time-of-check / time-of-use window: between the drop and the daemon's own
//! `bind`, a concurrently-running test can be handed the same ephemeral port.
//!
//! The oc-rsync daemon binds its default single listener with `SO_REUSEADDR`
//! only (matching upstream `socket.c:447`), so such a collision is a **clean
//! `EADDRINUSE` daemon-startup failure** - never a silent co-bind. (Only the
//! opt-in `acceptor threads > 1` multi-acceptor daemon sets `SO_REUSEPORT`, for
//! its own replica sockets.) That means a losing daemon simply exits, and the
//! winner owns the port exclusively - no two daemons ever share a port and no
//! cross-connection load-balancing is possible.
//!
//! [`spawn_daemon_on_free_port`] turns that into a bounded retry: allocate a
//! candidate port, spawn the daemon on it, and confirm the spawned process
//! itself is listening on that port (matched by pid via [`daemon_listen_port`]).
//! If the daemon lost the bind race it exits and we retry with a fresh port.
//! Matching by pid - rather than merely connecting to the port - is what closes
//! the window: it never mistakes a *different* winning daemon on the same port
//! for our own.

use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

/// How long to wait for a freshly-spawned daemon to bind its listener before
/// treating the attempt as failed and retrying with a new port.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on port-allocation retries, so a persistently failing daemon
/// (bad config, unbindable address) surfaces an error instead of looping.
const MAX_ATTEMPTS: u32 = 32;

/// Allocates a currently-free loopback TCP port by binding ephemeral port 0 and
/// dropping the listener. The port is free on return; callers must hand it to a
/// daemon promptly and tolerate the (now clean-failing) collision window via
/// the retry in [`spawn_daemon_on_free_port`].
fn candidate_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Spawns a daemon on a race-free free port and returns the live child plus the
/// port it owns.
///
/// `spawn_on_port(port)` must start the daemon process bound to `port` (e.g.
/// `oc-rsync --daemon --port <port> ...`) and return its [`Child`]. The helper
/// allocates candidate ports and retries until the spawned process is confirmed
/// listening on its port, or the attempt budget is exhausted.
///
/// The caller owns the returned [`Child`] and is responsible for reaping it
/// (typically via a guard whose `Drop` kills and waits).
pub fn spawn_daemon_on_free_port<F>(mut spawn_on_port: F) -> io::Result<(Child, u16)>
where
    F: FnMut(u16) -> io::Result<Child>,
{
    let mut last_err: Option<io::Error> = None;
    for _ in 0..MAX_ATTEMPTS {
        let Some(port) = candidate_port() else {
            continue;
        };
        let mut child = spawn_on_port(port)?;
        let pid = child.id();
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut acquired = false;
        loop {
            // Confirm *this* process is the one listening on `port`. On a lost
            // bind race the daemon never reaches this state (it exits with
            // EADDRINUSE), so a different winning daemon on the same port is
            // never mistaken for ours.
            if daemon_listen_port(pid) == Some(port) {
                acquired = true;
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    last_err = Some(io::Error::other(format!(
                        "daemon exited before binding port {port} ({status})"
                    )));
                    break;
                }
                Ok(None) => {}
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                last_err = Some(io::Error::other(format!(
                    "daemon did not bind port {port} within {READY_TIMEOUT:?}"
                )));
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if acquired {
            return Ok((child, port));
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("could not allocate a free daemon port")))
}

/// Returns the TCP port that process `pid` is listening on (loopback), or
/// `None` if it has no `LISTEN` TCP socket yet.
///
/// Used by [`spawn_daemon_on_free_port`] to confirm a specific daemon process
/// owns its port. Only the daemon's own socket table is consulted (matched by
/// the socket inodes it owns on Linux, or scoped to the pid by `lsof` on
/// macOS), so an unrelated process listening on loopback is never mistaken for
/// the daemon.
#[cfg(target_os = "linux")]
#[must_use]
pub fn daemon_listen_port(pid: u32) -> Option<u16> {
    use std::collections::HashSet;
    use std::fs;

    let mut inodes: HashSet<String> = HashSet::new();
    for entry in fs::read_dir(format!("/proc/{pid}/fd")).ok()?.flatten() {
        if let Ok(link) = fs::read_link(entry.path()) {
            if let Some(inode) = link
                .to_str()
                .and_then(|s| s.strip_prefix("socket:["))
                .and_then(|s| s.strip_suffix(']'))
            {
                inodes.insert(inode.to_owned());
            }
        }
    }
    if inodes.is_empty() {
        return None;
    }

    // `/proc/<pid>/net/tcp{,6}` columns: sl local_address rem_address st ...
    // inode. `st == 0A` is TCP_LISTEN; `local_address` is HEXIP:HEXPORT.
    for family in ["tcp", "tcp6"] {
        let Ok(table) = fs::read_to_string(format!("/proc/{pid}/net/{family}")) else {
            continue;
        };
        for line in table.lines().skip(1) {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 10 || cols[3] != "0A" || !inodes.contains(cols[9]) {
                continue;
            }
            if let Some((_, port_hex)) = cols[1].rsplit_once(':') {
                if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                    if port != 0 {
                        return Some(port);
                    }
                }
            }
        }
    }
    None
}

/// macOS variant backed by `lsof`, which is present by default.
#[cfg(target_os = "macos")]
#[must_use]
pub fn daemon_listen_port(pid: u32) -> Option<u16> {
    let output = std::process::Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-p",
            &pid.to_string(),
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
        ])
        .output()
        .ok()?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        // e.g. "n127.0.0.1:52345" or "n[::1]:52345".
        if let Some((_, port)) = line.strip_prefix('n').and_then(|a| a.rsplit_once(':')) {
            if let Ok(p) = port.parse::<u16>() {
                if p != 0 {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// Fallback for platforms without a supported discovery mechanism. The daemon
/// integration tests using this are `#![cfg(unix)]` and CI runs them only on
/// Linux and macOS.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[must_use]
pub fn daemon_listen_port(_pid: u32) -> Option<u16> {
    None
}
