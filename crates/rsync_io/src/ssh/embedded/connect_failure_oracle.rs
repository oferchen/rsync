//! Oracle cells: every embedded connect failure's stderr against real `ssh`'s.
//!
//! Each cell builds one failure, points the oracle `ssh` and the embedded
//! transport at it, and compares the exact bytes `ssh` writes to stderr with
//! [`SshError::ssh_stderr`](super::error::SshError::ssh_stderr) - CRLF line
//! endings included. The host-key cells run against an in-process russh
//! server; the authentication cell runs against a live `sshd`.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use russh::keys::{Algorithm, PrivateKey, PublicKey};

use super::config::SshConfig;
use super::connect::connect_and_exec;
use super::types::StrictHostKeyChecking;

/// Environment switch that turns a missing oracle `ssh` into a failure.
const REQUIRE_ORACLE: &str = "OC_RSYNC_REQUIRE_SSH_G";

/// The oracle: `OC_RSYNC_SSH_G_BIN`, else the first `ssh` on `PATH`.
fn oracle_ssh() -> Option<PathBuf> {
    if let Some(bin) = env::var_os("OC_RSYNC_SSH_G_BIN") {
        return Some(bin.into());
    }
    env::split_paths(&env::var_os("PATH")?)
        .map(|dir| dir.join("ssh"))
        .find(|candidate| candidate.is_file())
}

/// The oracle, or `None` after saying why the cell is skipped. A skip under
/// [`REQUIRE_ORACLE`] fails instead, so a missing oracle never reads as a pass.
fn oracle_or_skip(cell: &str) -> Option<PathBuf> {
    let ssh = oracle_ssh();
    if ssh.is_none() {
        assert!(
            env::var_os(REQUIRE_ORACLE).is_none(),
            "{cell}: {REQUIRE_ORACLE} is set but no oracle ssh was found"
        );
        eprintln!("skipping {cell}: no oracle ssh");
    }
    ssh
}

/// What the oracle writes to stderr for `args`; it must exit 255.
fn ssh_stderr(ssh: &Path, args: &[&str]) -> String {
    let output = Command::new(ssh)
        .args(["-F", "/dev/null", "-o", "BatchMode=yes"])
        .args(["-o", "GlobalKnownHostsFile=/dev/null"])
        .args(args)
        .args(["127.0.0.1", "true"])
        .stdin(Stdio::null())
        .output()
        .expect("run oracle ssh");
    assert_eq!(output.status.code(), Some(255), "oracle ssh {args:?}");
    String::from_utf8(output.stderr).expect("oracle stderr is UTF-8")
}

/// What the embedded transport reports for `config`; the connect must fail.
fn oc_stderr(config: &SshConfig) -> String {
    match connect_and_exec(config, "true", None) {
        Ok(_) => panic!("the embedded connect must fail"),
        Err(error) => error.ssh_stderr(),
    }
}

fn current_user() -> String {
    env::var("USER").expect("USER is set")
}

/// A config that mirrors the oracle's flags: batch mode, strict checking,
/// one user `known_hosts` file, no global files, no agent and no keys.
fn config(port: u16, known_hosts: &Path) -> SshConfig {
    SshConfig {
        host: "127.0.0.1".to_owned(),
        port,
        username: Some(current_user()),
        use_agent: false,
        identity_files: Vec::new(),
        batch_mode: true,
        user_known_hosts_files: Some(vec![known_hosts.to_path_buf()]),
        global_known_hosts_files: Vec::new(),
        strict_host_key_checking: StrictHostKeyChecking::Yes,
        connect_timeout: Duration::from_secs(10),
        ..SshConfig::default()
    }
}

fn known_hosts_arg(path: &Path) -> String {
    format!("UserKnownHostsFile={}", path.display())
}

/// A port nothing listens on: bound, then released.
fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}

/// A TCP server that runs `serve` on every connection it accepts.
fn tcp_server(serve: fn(std::net::TcpStream)) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || serve(stream));
        }
    });
    port
}

/// An SSH server with a fresh Ed25519 host key that rejects every login.
fn ssh_server() -> (u16, PublicKey) {
    struct RejectAll;
    impl russh::server::Server for RejectAll {
        type Handler = Self;
        fn new_client(&mut self, _peer: Option<SocketAddr>) -> Self {
            Self
        }
    }
    impl russh::server::Handler for RejectAll {
        type Error = russh::Error;
    }

    let host_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("keygen");
    let public = host_key.public_key().clone();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        ..Default::default()
    });
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let _ = russh::server::Server::run_on_socket(&mut RejectAll, config, &listener).await;
        });
    });
    (port, public)
}

fn known_hosts_line(port: u16, key: &PublicKey) -> String {
    format!(
        "[127.0.0.1]:{port} {}\n",
        key.to_openssh().expect("openssh key")
    )
}

/// openssh/sshconnect.c:525-526.
#[test]
fn refused_connection_matches_ssh() {
    let Some(ssh) = oracle_or_skip("refused_connection_matches_ssh") else {
        return;
    };
    let dir = test_support::create_tempdir();
    let known_hosts = dir.path().join("known_hosts");
    let port = closed_port();
    let expected = ssh_stderr(&ssh, &["-p", &port.to_string()]);
    assert_eq!(oc_stderr(&config(port, &known_hosts)), expected);
}

/// openssh/kex.c:1299-1301 and openssh/packet.c `SSH_ERR_CONN_TIMEOUT`.
#[test]
fn silent_server_banner_timeout_matches_ssh() {
    let Some(ssh) = oracle_or_skip("silent_server_banner_timeout_matches_ssh") else {
        return;
    };
    let port = tcp_server(|mut stream| {
        // Hold the connection open without ever sending a banner.
        let _ = std::io::copy(&mut stream, &mut std::io::sink());
    });
    let dir = test_support::create_tempdir();
    let known_hosts = dir.path().join("known_hosts");
    let expected = ssh_stderr(&ssh, &["-o", "ConnectTimeout=1", "-p", &port.to_string()]);
    let mut config = config(port, &known_hosts);
    config.connect_timeout = Duration::from_secs(1);
    assert_eq!(oc_stderr(&config), expected);
}

/// openssh/packet.c `SSH_ERR_CONN_CLOSED`: the server reads the client's
/// banner, then closes cleanly.
#[test]
fn server_closing_after_the_banner_matches_ssh() {
    let Some(ssh) = oracle_or_skip("server_closing_after_the_banner_matches_ssh") else {
        return;
    };
    let port = tcp_server(|stream| {
        let mut reader = BufReader::new(stream);
        let mut banner = String::new();
        let _ = reader.read_line(&mut banner);
        let _ = reader.get_ref().shutdown(Shutdown::Write);
        let _ = std::io::copy(&mut reader, &mut std::io::sink());
    });
    let dir = test_support::create_tempdir();
    let known_hosts = dir.path().join("known_hosts");
    let expected = ssh_stderr(&ssh, &["-p", &port.to_string()]);
    assert_eq!(oc_stderr(&config(port, &known_hosts)), expected);
}

/// openssh/sshconnect.c:1169-1178 and openssh/sshconnect2.c:102-103.
#[test]
fn unknown_host_key_under_strict_checking_matches_ssh() {
    let Some(ssh) = oracle_or_skip("unknown_host_key_under_strict_checking_matches_ssh") else {
        return;
    };
    let (port, _key) = ssh_server();
    let dir = test_support::create_tempdir();
    let known_hosts = dir.path().join("known_hosts");
    std::fs::write(&known_hosts, b"").expect("write known_hosts");
    let expected = ssh_stderr(
        &ssh,
        &[
            "-o",
            &known_hosts_arg(&known_hosts),
            "-o",
            "StrictHostKeyChecking=yes",
            "-p",
            &port.to_string(),
        ],
    );
    assert_eq!(oc_stderr(&config(port, &known_hosts)), expected);
}

/// openssh/sshconnect.c:1309-1331 `HOST_CHANGED` and :1667-1687
/// `warn_changed_key()`.
#[test]
fn changed_host_key_matches_ssh() {
    let Some(ssh) = oracle_or_skip("changed_host_key_matches_ssh") else {
        return;
    };
    let (port, _key) = ssh_server();
    let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("keygen");
    let dir = test_support::create_tempdir();
    let known_hosts = dir.path().join("known_hosts");
    std::fs::write(&known_hosts, known_hosts_line(port, other.public_key()))
        .expect("write known_hosts");
    let expected = ssh_stderr(
        &ssh,
        &[
            "-o",
            &known_hosts_arg(&known_hosts),
            "-o",
            "StrictHostKeyChecking=yes",
            "-p",
            &port.to_string(),
        ],
    );
    assert_eq!(oc_stderr(&config(port, &known_hosts)), expected);
}

/// openssh/sshconnect.c:1517-1528 and openssh/sshconnect2.c:102-103.
#[test]
fn revoked_host_key_matches_ssh() {
    let Some(ssh) = oracle_or_skip("revoked_host_key_matches_ssh") else {
        return;
    };
    let (port, key) = ssh_server();
    let dir = test_support::create_tempdir();
    let known_hosts = dir.path().join("known_hosts");
    std::fs::write(&known_hosts, b"").expect("write known_hosts");
    let revoked = dir.path().join("revoked");
    std::fs::write(&revoked, key.to_openssh().expect("openssh key")).expect("write revoked");
    let expected = ssh_stderr(
        &ssh,
        &[
            "-o",
            &known_hosts_arg(&known_hosts),
            "-o",
            &format!("RevokedHostKeys={}", revoked.display()),
            "-o",
            "StrictHostKeyChecking=yes",
            "-p",
            &port.to_string(),
        ],
    );
    let mut config = config(port, &known_hosts);
    config.revoked_host_keys = Some(revoked);
    assert_eq!(oc_stderr(&config), expected);
}

/// A live `sshd`, stopped when dropped.
struct Sshd {
    child: Child,
    port: u16,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `OC_RSYNC_SSHD_BIN`, else the usual install paths.
fn sshd_binary() -> Option<PathBuf> {
    env::var_os("OC_RSYNC_SSHD_BIN")
        .map(PathBuf::from)
        .or_else(|| {
            ["/usr/sbin/sshd", "/usr/bin/sshd"]
                .iter()
                .map(PathBuf::from)
                .find(|path| path.is_file())
        })
}

/// Starts an unprivileged `sshd` that offers only `publickey` and accepts
/// no key, returning once it reports that it is listening.
fn start_sshd(sshd: &Path, dir: &Path) -> Sshd {
    let host_key = dir.join("host_key");
    let generated = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&host_key)
        .status()
        .expect("run ssh-keygen");
    assert!(generated.success(), "ssh-keygen failed");
    let port = closed_port();
    let config = dir.join("sshd_config");
    std::fs::write(
        &config,
        format!(
            "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nPidFile none\nUsePAM no\n\
             PasswordAuthentication no\nKbdInteractiveAuthentication no\n\
             AuthorizedKeysFile none\nStrictModes no\n",
            host_key.display()
        ),
    )
    .expect("write sshd_config");
    let mut child = Command::new(sshd)
        .arg("-D")
        .arg("-e")
        .arg("-f")
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sshd");
    let stderr = child.stderr.take().expect("sshd stderr");
    let mut lines = BufReader::new(stderr).lines();
    loop {
        let line = lines
            .next()
            .expect("sshd exited before listening")
            .expect("read sshd stderr");
        if line.starts_with("Server listening on") {
            break;
        }
    }
    // Keep draining so sshd never blocks on a full pipe.
    std::thread::spawn(move || lines.for_each(drop));
    Sshd { child, port }
}

/// openssh/sshconnect2.c:541-543, against a live `sshd`.
#[test]
fn rejected_authentication_matches_ssh() {
    let Some(ssh) = oracle_or_skip("rejected_authentication_matches_ssh") else {
        return;
    };
    let Some(sshd_bin) = sshd_binary() else {
        eprintln!("skipping rejected_authentication_matches_ssh: no sshd");
        return;
    };
    let dir = test_support::create_tempdir();
    let sshd = start_sshd(&sshd_bin, dir.path());
    let host_public = std::fs::read_to_string(dir.path().join("host_key.pub"))
        .expect("read host key")
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    let known_hosts = dir.path().join("known_hosts");
    let mut entry = std::fs::File::create(&known_hosts).expect("create known_hosts");
    writeln!(entry, "[127.0.0.1]:{} {host_public}", sshd.port).expect("write known_hosts");
    let identity = dir.path().join("id_ed25519");
    let unauthorized = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("keygen");
    unauthorized
        .write_openssh_file(&identity, russh::keys::ssh_key::LineEnding::LF)
        .expect("write identity");

    let expected = ssh_stderr(
        &ssh,
        &[
            "-o",
            &known_hosts_arg(&known_hosts),
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "IdentitiesOnly=yes",
            "-o",
            "IdentityAgent=none",
            "-i",
            &identity.display().to_string(),
            "-l",
            &current_user(),
            "-p",
            &sshd.port.to_string(),
        ],
    );
    let mut config = config(sshd.port, &known_hosts);
    config.identity_files = vec![identity];
    assert_eq!(oc_stderr(&config), expected);
}
