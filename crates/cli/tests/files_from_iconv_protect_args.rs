//! A `--files-from` list carried over the connection is converted with
//! `--iconv` only under `--protect-args` (`-s`).
//!
//! upstream: compat.c:820-827 - `if (protect_args && files_from)` sets
//! `filesfrom_convert` (`filesfrom_host && ic_send != -1` on the sender,
//! `!filesfrom_host && ic_recv != -1` on the receiver). Without it the
//! forwarding side (io.c:502) and the reading side (flist.c:2766) leave the
//! names as written, so the sender looks up the unconverted spelling.
//!
//! The client runs `--iconv=UTF-8,ISO-8859-1`: its tree and list are UTF-8,
//! the server's are ISO-8859-1 (one leg swaps the two). Every cell asserts the rsync 3.5.2 result
//! measured on glibc. When the 3.5.2 oracle resolves
//! (`OC_RSYNC_UPSTREAM_BIN_3_5_2`, required under
//! `OC_RSYNC_UPSTREAM_COMPAT=1`), each cell also runs 3.5.2 at both ends and
//! with one end each, and all results must be equal.
#![cfg(unix)]

use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use test_support::{UpstreamVersion, oc_rsync_bin, require_upstream_rsync};

/// `café` in the client's UTF-8.
const UTF8_NAME: &[u8] = b"caf\xC3\xA9";
/// `café` in the server's ISO-8859-1.
const LATIN1_NAME: &[u8] = b"caf\xE9";

#[derive(Clone, Copy, Debug)]
enum Leg {
    /// The server sends; the client forwards its own UTF-8 list.
    Pull,
    /// The client sends; the server forwards its own ISO-8859-1 list
    /// (`--files-from=:list`).
    Push,
    /// A daemon module with `charset = ISO-8859-1` sends; the client forwards
    /// its own UTF-8 list.
    DaemonPull,
    /// [`Leg::Push`] with the charsets swapped (`--iconv=ISO-8859-1,UTF-8`):
    /// the client tree is ISO-8859-1 and the server forwards a UTF-8 list, so
    /// the client sender itself must convert what it reads (flist.c:2766).
    PushFromLatin1Client,
}

/// The names directly below `dir`, sorted, as raw bytes.
fn names(dir: &Path) -> Vec<Vec<u8>> {
    let mut names: Vec<Vec<u8>> = fs::read_dir(dir)
        .expect("read destination")
        .map(|entry| entry.expect("entry").file_name().into_vec())
        .collect();
    names.sort();
    names
}

/// Runs one transfer and returns its exit code with the destination names.
fn run(client: &Path, server: &Path, leg: Leg, protect_args: bool) -> (Option<i32>, Vec<Vec<u8>>) {
    let dir = TempDir::new().expect("tempdir");
    let local = dir.path().join("local");
    let remote = dir.path().join("remote");
    fs::create_dir(&local).expect("mkdir local");
    fs::create_dir(&remote).expect("mkdir remote");
    let shell = dir.path().join("rsh");
    fs::write(&shell, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write shell");
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o755)).expect("chmod shell");
    let list = dir.path().join("list");
    // The daemon, started through a remote shell.
    let daemon_shell = dir.path().join("daemon-rsh");
    let conf = dir.path().join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "use chroot = no\n[src]\npath = {}\ncharset = ISO-8859-1\n",
            remote.display()
        ),
    )
    .expect("write config");
    fs::write(
        &daemon_shell,
        format!(
            "#!/bin/sh\nexec \"{}\" --config={} --server --daemon .\n",
            server.display(),
            conf.display()
        ),
    )
    .expect("write daemon shell");
    fs::set_permissions(&daemon_shell, fs::Permissions::from_mode(0o755))
        .expect("chmod daemon shell");

    let mut command = Command::new(client);
    let iconv = if matches!(leg, Leg::PushFromLatin1Client) {
        "--iconv=ISO-8859-1,UTF-8"
    } else {
        "--iconv=UTF-8,ISO-8859-1"
    };
    command
        .args(["-r", iconv])
        .arg(if protect_args { "-s" } else { "--no-s" })
        .arg("-e");
    if matches!(leg, Leg::DaemonPull) {
        command.arg(&daemon_shell);
    } else {
        command
            .arg(&shell)
            .arg(format!("--rsync-path={}", server.display()));
    }
    let destination = match leg {
        Leg::DaemonPull => {
            fs::write(
                remote.join(std::ffi::OsStr::from_bytes(LATIN1_NAME)),
                b"x\n",
            )
            .expect("write source file");
            fs::write(&list, [UTF8_NAME, b"\n"].concat()).expect("write list");
            command
                .arg(format!("--files-from={}", list.display()))
                .arg("h::src/")
                .arg(format!("{}/", local.display()));
            local
        }
        Leg::Pull => {
            fs::write(
                remote.join(std::ffi::OsStr::from_bytes(LATIN1_NAME)),
                b"x\n",
            )
            .expect("write source file");
            fs::write(&list, [UTF8_NAME, b"\n"].concat()).expect("write list");
            command
                .arg(format!("--files-from={}", list.display()))
                .arg(format!("h:{}/", remote.display()))
                .arg(format!("{}/", local.display()));
            local
        }
        Leg::Push => {
            fs::write(local.join(std::ffi::OsStr::from_bytes(UTF8_NAME)), b"x\n")
                .expect("write source file");
            fs::write(&list, [LATIN1_NAME, b"\n"].concat()).expect("write list");
            command
                .arg(format!("--files-from=:{}", list.display()))
                .arg(format!("{}/", local.display()))
                .arg(format!("h:{}/", remote.display()));
            remote
        }
        Leg::PushFromLatin1Client => {
            fs::write(local.join(std::ffi::OsStr::from_bytes(LATIN1_NAME)), b"x\n")
                .expect("write source file");
            fs::write(&list, [UTF8_NAME, b"\n"].concat()).expect("write list");
            command
                .arg(format!("--files-from=:{}", list.display()))
                .arg(format!("{}/", local.display()))
                .arg(format!("h:{}/", remote.display()));
            remote
        }
    };
    let output = command.output().expect("run rsync");
    (output.status.code(), names(&destination))
}

/// Checks one leg: oc at both ends gives the measured 3.5.2 result, and so
/// does every pairing with 3.5.2.
fn check(leg: Leg, protect_args: bool, expected: (Option<i32>, Vec<Vec<u8>>)) {
    let cell = format!("{leg:?} protect_args={protect_args}");
    let oc = oc_rsync_bin();
    assert_eq!(
        run(&oc, &oc, leg, protect_args),
        expected,
        "{cell}: oc to oc"
    );
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_2) else {
        return;
    };
    let reference = upstream.binary();
    for (client, server, pairing) in [
        (reference, reference, "rsync 3.5.2 to rsync 3.5.2"),
        (reference, oc.as_path(), "rsync 3.5.2 client, oc server"),
        (oc.as_path(), reference, "oc client, rsync 3.5.2 server"),
    ] {
        // An oc client's `-s` request to a 3.5.2 daemon is a separate client
        // defect (the daemon reads the list unconverted); this file covers the
        // server side, which the other three pairings exercise.
        if matches!(leg, Leg::DaemonPull)
            && protect_args
            && server == reference
            && client == oc.as_path()
        {
            continue;
        }
        assert_eq!(
            run(client, server, leg, protect_args),
            expected,
            "{cell}: {pairing}"
        );
    }
}

/// Without `-s` the server sender looks up the UTF-8 spelling it was sent,
/// misses the ISO-8859-1 file and the transfer exits 23.
#[test]
fn a_pull_without_protect_args_sends_the_list_unconverted() {
    check(Leg::Pull, false, (Some(23), Vec::new()));
}

#[test]
fn a_pull_with_protect_args_converts_the_list() {
    check(Leg::Pull, true, (Some(0), vec![UTF8_NAME.to_vec()]));
}

/// Without `-s` the client sender looks up the ISO-8859-1 spelling the server
/// forwarded and misses its UTF-8 file.
#[test]
fn a_push_without_protect_args_forwards_the_remote_list_unconverted() {
    check(Leg::Push, false, (Some(23), Vec::new()));
}

/// Under `-s` the server receiver converts the list it opened before
/// forwarding it (io.c:502), so the client sender finds its UTF-8 file.
#[test]
fn a_push_with_protect_args_converts_the_remote_list() {
    check(Leg::Push, true, (Some(0), vec![LATIN1_NAME.to_vec()]));
}

/// A daemon learns protect_args from the client's `-s` (clientserver.c:1165)
/// and applies its module `charset` the same way.
#[test]
fn a_daemon_pull_without_protect_args_sends_the_list_unconverted() {
    check(Leg::DaemonPull, false, (Some(23), Vec::new()));
}

#[test]
fn a_daemon_pull_with_protect_args_converts_the_list() {
    check(Leg::DaemonPull, true, (Some(0), vec![UTF8_NAME.to_vec()]));
}

/// Without `-s` the client sender looks up the UTF-8 spelling it read and
/// misses its ISO-8859-1 file.
#[test]
fn a_latin1_client_push_without_protect_args_reads_the_list_unconverted() {
    check(Leg::PushFromLatin1Client, false, (Some(23), Vec::new()));
}

/// Under `-s` the client sender converts the UTF-8 list into its own charset
/// before the lookup.
#[test]
fn a_latin1_client_push_with_protect_args_converts_the_list() {
    check(
        Leg::PushFromLatin1Client,
        true,
        (Some(0), vec![UTF8_NAME.to_vec()]),
    );
}
