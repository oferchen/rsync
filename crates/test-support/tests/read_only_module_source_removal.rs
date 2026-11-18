//! A daemon never lets a client remove files from a module it may only read.
//!
//! `--remove-source-files` makes the sending side unlink each source once the
//! receiver confirms it. On a pull from a daemon the sender is the daemon, so
//! rsync 3.5.2 refuses the option outright when the module is read-only for
//! this client, before it lists or opens a file:
//!
//! - main.c:966-971 `do_server_sender()` - `ERROR: --remove-%s-files cannot be
//!   used with a read-only module`, `RERR_SYNTAX`. `%s` is `source` or, for the
//!   deprecated `--remove-sent-files`, `sent`.
//! - `read_only` is the module's `read only` setting (true unless the module
//!   says otherwise, clientserver.c:809) as adjusted by the authenticated
//!   user's `:ro` / `:rw` suffix (authenticate.c:434-436).
//! - `refuse options = delete` refuses both spellings on a sender too
//!   (options.c:2424-2431), with the `--delete` refusal line.
//!
//! Every cell runs against rsync 3.5.2 on both ends, oc-rsync on both ends, and
//! each mix of the two, over `rsync://` and over a daemon reached through a
//! remote shell. It compares whether the run succeeded, the first error line,
//! whether the module file survived and whether anything was copied; on
//! `rsync://` the read-only cells also compare the client's exit code.
//!
//! Not compared: the lines after the first and, outside those cells, the exact
//! non-zero exit code. An oc daemon ends a refused session differently from
//! rsync 3.5.2 there (as it already does for `ERROR: module is write only`),
//! which is a property of its refusal framing, not of this check.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use test_support::inetd_server::{InetdCommand, InetdServer};
use test_support::{UpstreamVersion, create_tempdir, oc_rsync_bin, require_upstream_rsync};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Transport {
    /// `rsync://host:port/module/`.
    Tcp,
    /// `host::module/` through a remote shell that runs `--server --daemon`.
    RemoteShell,
}

/// What rsync 3.5.2 does with a removal request on one module.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Expect {
    /// Refused because the module is read-only for this client.
    ReadOnly,
    /// Refused by `refuse options = delete`.
    RefusedDelete,
    /// The client may write here: the file is copied and then removed.
    Allowed,
}

/// The modules of the fixture's `rsyncd.conf`, all serving the same directory.
const MODULES: &[(&str, Expect)] = &[
    // `read only = yes`.
    ("explicit", Expect::ReadOnly),
    // No `read only` line: the default is read-only.
    ("default", Expect::ReadOnly),
    // `read only = no`, but the user is listed as `u:ro`.
    ("authro", Expect::ReadOnly),
    // `read only = yes`, but the user is listed as `u:rw`.
    ("authrw", Expect::Allowed),
    // `read only = no` with `refuse options = delete`.
    ("refuse", Expect::RefusedDelete),
    // `read only = no`.
    ("rw", Expect::Allowed),
];

const SPELLINGS: &[&str] = &["source", "sent"];
const TRANSPORTS: &[Transport] = &[Transport::Tcp, Transport::RemoteShell];

/// Whether the client succeeded, its exit code, its first stderr line, whether
/// the module's file survived and whether the client received a copy.
#[derive(Debug, PartialEq)]
struct Outcome {
    succeeded: bool,
    exit: Option<i32>,
    first_error: String,
    source_kept: bool,
    copied: bool,
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
}

/// Lays out `root/module/src/a`, an empty `root/dst`, the daemon config, its
/// secrets file, and the two scripts the remote-shell leg needs: a shell that
/// drops `-l user` and the host name, and a server path that adds `--config`.
fn seed(root: &Path, daemon: &Path) {
    fs::create_dir_all(root.join("module/src")).expect("mkdir module");
    fs::create_dir_all(root.join("dst")).expect("mkdir dst");
    fs::write(root.join("module/src/a"), "a\n").expect("seed file");
    let secrets = root.join("secrets");
    fs::write(&secrets, "u:pw\n").expect("write secrets");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o600)).expect("chmod secrets");
    let (dir, module) = (root.display(), root.join("module"));
    let module = module.display();
    fs::write(
        root.join("rsyncd.conf"),
        format!(
            "use chroot = no\npid file = {dir}/rsyncd.pid\n\
             [explicit]\npath = {module}\nread only = yes\n\
             [default]\npath = {module}\n\
             [authro]\npath = {module}\nread only = no\n\
             auth users = u:ro\nsecrets file = {dir}/secrets\n\
             [authrw]\npath = {module}\nread only = yes\n\
             auth users = u:rw\nsecrets file = {dir}/secrets\n\
             [refuse]\npath = {module}\nread only = no\nrefuse options = delete\n\
             [rw]\npath = {module}\nread only = no\n"
        ),
    )
    .expect("write rsyncd.conf");
    write_executable(
        &root.join("rsh"),
        "#!/bin/sh\nif [ \"$1\" = -l ]; then shift 2; fi\nshift\nexec \"$@\"\n",
    );
    write_executable(
        &root.join("server"),
        &format!(
            "#!/bin/sh\nexec '{}' --config='{dir}/rsyncd.conf' \"$@\"\n",
            daemon.display()
        ),
    );
}

/// Pulls `module/src/` into `dst/` with `--remove-<spelling>-files`, `client`
/// on the near end and `daemon` serving the module.
fn pull(
    client: &Path,
    daemon: &Path,
    transport: Transport,
    module: &str,
    spelling: &str,
) -> Outcome {
    let tmp = create_tempdir();
    let root = tmp.path();
    seed(root, daemon);
    let user = if module.starts_with("auth") { "u@" } else { "" };
    let mut command = Command::new(client);
    command
        .current_dir(root)
        .env("RSYNC_PASSWORD", "pw")
        .arg("-a")
        .arg(format!("--remove-{spelling}-files"));
    let server = match transport {
        Transport::Tcp => {
            let stderr = fs::OpenOptions::new()
                .write(true)
                .open("/dev/null")
                .expect("open /dev/null");
            let listener =
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind daemon listener");
            let inetd = InetdCommand::new(daemon, stderr)
                .arg(format!("--config={}/rsyncd.conf", root.display()));
            let server = InetdServer::start(listener, inetd).expect("start daemon");
            command.arg(format!(
                "rsync://{user}127.0.0.1:{}/{module}/src/",
                server.port()
            ));
            Some(server)
        }
        Transport::RemoteShell => {
            command
                .arg("-e")
                .arg(root.join("rsh"))
                .arg("--rsync-path")
                .arg(root.join("server"))
                .arg(format!("{user}host::{module}/src/"));
            None
        }
    };
    let out = command.arg("dst/").output().expect("run client");
    if let Some(server) = server {
        server.finish().expect("daemon sessions ended");
    }
    Outcome {
        succeeded: out.status.success(),
        exit: out.status.code(),
        first_error: String::from_utf8_lossy(&out.stderr)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned(),
        source_kept: root.join("module/src/a").exists(),
        copied: root.join("dst/a").exists(),
    }
}

/// The part of an outcome every pairing must reproduce.
fn observable(
    outcome: &Outcome,
    transport: Transport,
    expect: Expect,
) -> impl PartialEq + std::fmt::Debug {
    let exit = (transport == Transport::Tcp && expect == Expect::ReadOnly).then_some(outcome.exit);
    (
        outcome.succeeded,
        outcome.first_error.clone(),
        outcome.source_kept,
        outcome.copied,
        exit,
    )
}

#[test]
fn a_read_only_module_refuses_source_removal_like_rsync_3_5_2() {
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let (up, oc) = (upstream.binary(), oc_rsync_bin());
    let mut differing = Vec::new();
    let mut cells = 0;
    for &transport in TRANSPORTS {
        for &(module, expect) in MODULES {
            for spelling in SPELLINGS {
                let cell = format!("{transport:?} [{module}] --remove-{spelling}-files");
                let theirs = pull(up, up, transport, module, spelling);
                let (first_error, refused) = match expect {
                    Expect::ReadOnly => (
                        format!(
                            "ERROR: --remove-{spelling}-files cannot be used with a \
                             read-only module"
                        ),
                        true,
                    ),
                    Expect::RefusedDelete => (
                        "rsync: The server is configured to refuse --delete".to_owned(),
                        true,
                    ),
                    Expect::Allowed => (String::new(), false),
                };
                assert_eq!(
                    (
                        theirs.succeeded,
                        theirs.first_error.as_str(),
                        theirs.source_kept,
                        theirs.copied
                    ),
                    (!refused, first_error.as_str(), refused, !refused),
                    "rsync 3.5.2, {cell}: {theirs:?}"
                );
                for (client, daemon, ends) in [
                    (&*oc, &*oc, "oc client, oc daemon"),
                    (up, &*oc, "3.5.2 client, oc daemon"),
                    (&*oc, up, "oc client, 3.5.2 daemon"),
                ] {
                    cells += 1;
                    let ours = pull(client, daemon, transport, module, spelling);
                    if observable(&ours, transport, expect)
                        != observable(&theirs, transport, expect)
                    {
                        differing.push(format!(
                            "{cell}, {ends}\n  rsync 3.5.2: {theirs:?}\n  this pairing: {ours:?}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        differing.is_empty(),
        "{} of {cells} cells differ from rsync 3.5.2:\n{}",
        differing.len(),
        differing.join("\n")
    );
}
