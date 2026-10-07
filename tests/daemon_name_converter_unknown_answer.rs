//! A daemon `name converter` that cannot map a sender's name must leave the
//! sender's numeric id in place. It must never turn the name into root.
//!
//! Upstream reads an id answer strictly:
//!
//! ```c
//! /* clientserver.c:1340-1354 - namecvt_call(), name-to-id branch */
//! if (!*buf)
//!         return False;
//! for (p = buf; *p; p++) {
//!         if (*p < '0' || *p > '9')
//!                 return False;
//! }
//! ```
//!
//! and the receiver falls back to the id the sender sent when the lookup fails:
//!
//! ```c
//! /* uidlist.c:273-276 - recv_add_id() */
//! else if (*name && id) {
//!         ...
//!         id2 = user_to_uid(name, &uid, False) ? (id_t)uid : id;
//! ```
//!
//! Before the fix, `*id_p = (id_t)atol(buf)` read an empty or non-numeric
//! answer as 0. Every name the converter did not know then became uid/gid 0 on
//! a root daemon (CVE-2026-53798). The bundled `support/nameconvert` helper
//! answers an empty line for any unknown name, so this is its normal answer.
//!
//! The upstream 3.5.1 control on the same fixture (root daemon, `uid = 0`,
//! source file owned by `nobody`) keeps the sender's ids for an empty answer
//! and for `notanumber`, and applies `4242` when the converter answers it.
//!
//! WHY THIS NEEDS ROOT. Only a root daemon can give a received file an owner
//! other than itself, so only a root daemon can show the mapping at all. The
//! upstream `daemon-namecvt-empty-response` cell runs unprivileged and reads
//! the `--fake-super` xattr instead; oc-rsync does not implement fake-super,
//! so that cell passes without exercising the mapping. Unprivileged runs of
//! this file print a reason and return.
#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use test_support::ReapOnDrop;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
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
/// `stdin` is `/dev/null` so the daemon listens instead of taking its inetd
/// path on an inherited terminal or pipe.
fn spawn_daemon(conf: &Path, port: u16) -> ReapOnDrop {
    let child = ReapOnDrop::new(
        Command::new(oc_binary())
            .arg("--daemon")
            .arg("--no-detach")
            .arg(format!("--config={}", conf.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
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
    sent: (u32, u32),
    received: Option<(u32, u32)>,
    requests: String,
}

/// Pushes one file owned by `nobody` into a root daemon whose name converter
/// prints `answer` for every `usr` and `grp` request.
///
/// Returns `None` when the host cannot stage the fixture (no `nobody` account,
/// no loopback port), which the callers report as a skip.
fn push_through_converter(answer: &str) -> Option<Outcome> {
    let nobody_gid = id_of(&["-g", "nobody"])?;
    let root = tempfile::tempdir().expect("temp dir");
    let port = free_port()?;

    let module = root.path().join("mod");
    let src = root.path().join("src");
    fs::create_dir_all(&module).expect("module dir");
    fs::create_dir_all(&src).expect("source dir");
    let source_file = src.join("f");
    fs::write(&source_file, b"payload\n").expect("source file");
    let chowned = Command::new("chown")
        .arg(format!("nobody:{nobody_gid}"))
        .arg(&source_file)
        .status()
        .is_ok_and(|status| status.success());
    if !chowned {
        return None;
    }

    // The converter logs every request so the cell can prove the daemon asked
    // it at all; a daemon that never consulted the converter would keep the
    // sender's ids for an unrelated reason.
    let requests = root.path().join("requests");
    let converter = root.path().join("cvt");
    fs::write(
        &converter,
        format!(
            "#!/bin/sh\n\
             while read cmd arg; do\n\
             \techo \"$cmd $arg\" >> '{log}'\n\
             \tcase \"$cmd\" in\n\
             \t\tusr|grp) echo '{answer}' ;;\n\
             \t\t*) echo '' ;;\n\
             \tesac\n\
             done\n",
            log = requests.display(),
        ),
    )
    .expect("converter script");
    fs::set_permissions(&converter, fs::Permissions::from_mode(0o755)).expect("chmod converter");

    // `uid = 0` / `gid = 0` keep the daemon root, so it can chown to whatever
    // the mapping says. Numeric values never reach the converter
    // (uidlist.c:149, `num_ok`).
    let conf = format!(
        "port = {port}\n\
         use chroot = no\n\
         \n\
         [m]\n\
         \tpath = {module}\n\
         \tread only = no\n\
         \tuid = 0\n\
         \tgid = 0\n\
         \tname converter = {converter}\n",
        module = module.display(),
        converter = converter.display(),
    );
    let conf_path = root.path().join("rsyncd.conf");
    fs::write(&conf_path, conf).expect("config");

    let daemon = spawn_daemon(&conf_path, port);
    let status = Command::new(oc_binary())
        .args([
            "-a",
            &format!("{}/", src.display()),
            &format!("rsync://127.0.0.1:{port}/m/"),
        ])
        .status()
        .expect("run client");
    let received = fs::metadata(module.join("f"))
        .ok()
        .map(|meta| (meta.uid(), meta.gid()));
    drop(daemon);
    assert!(status.success(), "the push itself must succeed: {status}");

    let sent = fs::metadata(&source_file).expect("source metadata");
    Some(Outcome {
        sent: (sent.uid(), sent.gid()),
        received,
        requests: fs::read_to_string(&requests).unwrap_or_default(),
    })
}

fn assert_sender_ids_kept(answer: &str) {
    if id_of(&["-u"]).as_deref() != Some("0") {
        println!("SKIP: needs root - only a root daemon can set a received file's owner");
        return;
    }
    let Some(outcome) = push_through_converter(answer) else {
        println!("SKIP: no `nobody` account or no loopback port");
        return;
    };

    assert!(
        outcome.requests.contains("usr nobody") && outcome.requests.contains("grp "),
        "the daemon must ask the converter for the sender's user and group \
         names, otherwise this cell proves nothing; requests: {:?}",
        outcome.requests
    );
    assert_ne!(
        outcome.sent,
        (0, 0),
        "the fixture needs a non-root sender, or a root mapping is invisible"
    );
    assert_eq!(
        outcome.received,
        Some(outcome.sent),
        "a converter answer of {answer:?} is a failed lookup \
         (clientserver.c:1345-1354), so the receiver must keep the sender's \
         ids (uidlist.c:276) - never map the name to uid/gid 0"
    );
}

/// The bundled `support/nameconvert` answers an empty line for an unknown name.
#[test]
fn empty_converter_answer_keeps_the_senders_ids() {
    assert_sender_ids_kept("");
}

/// `atol("notanumber") == 0` too, so a non-numeric answer is the same hazard.
#[test]
fn non_numeric_converter_answer_keeps_the_senders_ids() {
    assert_sender_ids_kept("notanumber");
}

/// Non-vacuity: a usable answer must actually drive the received ownership.
/// Without this, a daemon that ignored the converter (or never chowned) would
/// keep the sender's ids and pass the two cells above for the wrong reason.
#[test]
fn numeric_converter_answer_maps_the_received_ids() {
    if id_of(&["-u"]).as_deref() != Some("0") {
        println!("SKIP: needs root - only a root daemon can set a received file's owner");
        return;
    }
    let Some(outcome) = push_through_converter("4242") else {
        println!("SKIP: no `nobody` account or no loopback port");
        return;
    };
    assert_eq!(
        outcome.received,
        Some((4242, 4242)),
        "the converter's numeric answer must become the received owner; \
         requests: {:?}",
        outcome.requests
    );
}
