//! Client transports for the fidelity matrix.
//!
//! Each [`Transport`] runs oc-rsync (or upstream rsync) as the *pulling client*
//! over one code path: a local filesystem copy, an ssh subprocess (`host:path`),
//! the embedded russh client (`ssh://` URL), or an rsync daemon (`rsync://`).
//! For every network transport the *sender* is upstream rsync - via
//! `--rsync-path` for ssh/russh, and an upstream `--daemon` for `rsync://` - so
//! only the client under test varies between two runs of the same cell.

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::error::{TaskError, TaskResult};

/// A client transport oc-rsync supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// Plain local filesystem copy (`src/ dst/`).
    Local,
    /// SSH subprocess (`-e ssh host:path`).
    SshSubprocess,
    /// Embedded russh client (`ssh://host/path`).
    Russh,
    /// Rsync daemon (`rsync://host:port/module`).
    Daemon,
}

impl Transport {
    /// All transports, in matrix order.
    pub const ALL: [Transport; 4] = [
        Transport::Local,
        Transport::SshSubprocess,
        Transport::Russh,
        Transport::Daemon,
    ];

    /// Stable short label used in reports and CLI selection.
    pub fn label(self) -> &'static str {
        match self {
            Transport::Local => "local",
            Transport::SshSubprocess => "ssh-subprocess",
            Transport::Russh => "russh",
            Transport::Daemon => "daemon",
        }
    }

    /// Parse a CLI `--transport` value; `None` if unrecognized.
    pub fn parse(value: &str) -> Option<Transport> {
        if value.is_empty() {
            return None;
        }
        Transport::ALL
            .into_iter()
            .find(|t| t.label() == value || t.label().starts_with(value))
    }

    /// True when the transport needs a reachable sshd on localhost.
    pub fn needs_ssh(self) -> bool {
        matches!(self, Transport::SshSubprocess | Transport::Russh)
    }

    /// Transport the upstream reference should use for this cell.
    ///
    /// Upstream rsync has no embedded russh client, so the `ssh://` (russh) cell
    /// is validated against upstream over the ssh subprocess - an identical
    /// transfer, differing only in the client's SSH implementation.
    pub fn for_upstream(self) -> Transport {
        match self {
            Transport::Russh => Transport::SshSubprocess,
            other => other,
        }
    }
}

/// Pull `src/` into a fresh `dst/` using `client` over `transport`.
///
/// `flags` is the complete rsync flag set (e.g. `-rlptgoD -A -X`); the source
/// operand and `--rsync-path`/daemon plumbing are added per transport. The
/// sender is always `upstream` so only the client differs across runs. The
/// destination directory is (re)created empty before the transfer.
pub fn pull_into(
    transport: Transport,
    client: &Path,
    upstream: &Path,
    src: &Path,
    dst: &Path,
    flags: &[String],
    work: &Path,
) -> TaskResult<Output> {
    reset_dir(dst)?;
    let dst_arg = format!("{}/", dst.display());
    let mut cmd = Command::new(client);
    cmd.args(flags);

    match transport {
        Transport::Local => {
            cmd.arg(format!("{}/", src.display())).arg(&dst_arg);
        }
        Transport::SshSubprocess => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg("-e")
                .arg("ssh -o BatchMode=yes -o StrictHostKeyChecking=no")
                .arg(format!("localhost:{}/", src.display()))
                .arg(&dst_arg);
        }
        Transport::Russh => {
            cmd.arg(format!("--rsync-path={}", upstream.display()))
                .arg(format!("ssh://localhost{}/", src.display()))
                .arg(&dst_arg);
        }
        Transport::Daemon => {
            let daemon = DaemonHandle::start(upstream, src, work)?;
            cmd.arg(daemon.module_url()).arg(&dst_arg);
            let out = run(cmd)?;
            drop(daemon);
            return Ok(out);
        }
    }
    run(cmd)
}

/// Run a prepared command, capturing combined output.
fn run(mut cmd: Command) -> TaskResult<Output> {
    cmd.output()
        .map_err(|e| TaskError::Validation(format!("failed to spawn {cmd:?}: {e}")))
}

/// Recreate `dir` as an empty directory.
fn reset_dir(dir: &Path) -> TaskResult<()> {
    if dir.exists() {
        std::fs::remove_dir_all(dir)
            .map_err(|e| TaskError::Validation(format!("remove {}: {e}", dir.display())))?;
    }
    std::fs::create_dir_all(dir)
        .map_err(|e| TaskError::Validation(format!("create {}: {e}", dir.display())))
}

/// A short-lived upstream rsync daemon exporting one directory as module `m`.
///
/// Kills the process and removes its config on drop, so a check can start one
/// per transport cell without leaking daemons.
pub struct DaemonHandle {
    child: Child,
    port: u16,
    conf: PathBuf,
}

impl DaemonHandle {
    /// Start `daemon_bin --daemon` exporting `export_dir` (read-only, no chroot)
    /// on a free loopback port. Waits until the port accepts connections.
    pub fn start(daemon_bin: &Path, export_dir: &Path, work: &Path) -> TaskResult<DaemonHandle> {
        let port = free_port()?;
        let conf = work.join(format!("rsyncd-{port}.conf"));
        let body = format!(
            "use chroot = no\nport = {port}\n[m]\n    path = {}\n    read only = true\n    numeric ids = yes\n",
            export_dir.display()
        );
        std::fs::write(&conf, body)
            .map_err(|e| TaskError::Validation(format!("write daemon config: {e}")))?;

        let child = Command::new(daemon_bin)
            .args(["--daemon", "--no-detach", "--config"])
            .arg(&conf)
            .arg(format!("--port={port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| TaskError::Validation(format!("spawn daemon on {port}: {e}")))?;

        let handle = DaemonHandle { child, port, conf };
        if !wait_for_port(port, Duration::from_secs(5)) {
            return Err(TaskError::Validation(format!(
                "daemon did not open port {port} within 5s"
            )));
        }
        Ok(handle)
    }

    /// The `rsync://` URL for the exported module's contents.
    pub fn module_url(&self) -> String {
        format!("rsync://127.0.0.1:{}/m/", self.port)
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.conf);
    }
}

/// Pick a free TCP port on loopback by binding to port 0 and reading it back.
fn free_port() -> TaskResult<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|e| TaskError::Validation(format!("bind ephemeral port: {e}")))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| TaskError::Validation(format!("read ephemeral port: {e}")))
}

/// Poll until `port` accepts a connection on loopback, or the timeout elapses.
fn wait_for_port(port: u16, timeout: Duration) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            let _ = stream.write(&[]);
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::Transport;

    #[test]
    fn parse_accepts_exact_labels_and_unambiguous_prefixes() {
        assert_eq!(Transport::parse("local"), Some(Transport::Local));
        assert_eq!(Transport::parse("daemon"), Some(Transport::Daemon));
        assert_eq!(Transport::parse("russh"), Some(Transport::Russh));
        // Prefixes resolve to the first matching transport in ALL order.
        assert_eq!(Transport::parse("ssh"), Some(Transport::SshSubprocess));
        assert_eq!(Transport::parse("d"), Some(Transport::Daemon));
    }

    #[test]
    fn parse_rejects_empty_and_unknown() {
        assert_eq!(Transport::parse(""), None);
        assert_eq!(Transport::parse("nfs"), None);
    }

    #[test]
    fn every_label_round_trips_through_parse() {
        for transport in Transport::ALL {
            assert_eq!(Transport::parse(transport.label()), Some(transport));
        }
    }

    #[test]
    fn needs_ssh_is_true_only_for_ssh_paths() {
        assert!(Transport::SshSubprocess.needs_ssh());
        assert!(Transport::Russh.needs_ssh());
        assert!(!Transport::Local.needs_ssh());
        assert!(!Transport::Daemon.needs_ssh());
    }
}
