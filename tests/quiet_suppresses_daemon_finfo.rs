//! `-q` must silence the daemon MOTD and the module listing, like every other
//! `FINFO` line.
//!
//! upstream: clientserver.c:432-435 - `start_inband_exchange()` prints each
//! line between the greeting and `@RSYNCD: OK`/`EXIT` (MOTD or module row; the
//! two are indistinguishable) with `rprintf(FINFO, ...)`, and log.c:344-345
//! `rwrite()` drops every `FINFO` line under `quiet`. So `rsync -q rsync://host/`
//! prints nothing, and `rsync -q --list-only rsync://host/mod/` prints no MOTD.
//! Ground truth, rsync 3.5.1 against a daemon with `motd file` set: both print
//! zero lines under `-q`, `-qv` and `-q -vv`.
//!
//! The MOTD of a transfer is written straight to the process stdout, which only
//! a real child process can observe, so these tests run the shipped binary.
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::process::{Command, Output};
use std::thread;
use std::time::Duration;

const STUB_DEADLINE: Duration = Duration::from_secs(30);
const MOTD: &str = "quiet-test motd line";
const MODULE_ROW: &str = "quietmod       \tquiet-test comment";

fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// Serves one connection: greets, reads the client greeting and module request,
/// then replays `responses` verbatim.
fn spawn_stub_daemon(responses: Vec<String>) -> (SocketAddr, thread::JoinHandle<()>) {
    let listener =
        TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("bind stub daemon");
    let addr = listener.local_addr().expect("stub daemon address");
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accept client");
        stream
            .set_read_timeout(Some(STUB_DEADLINE))
            .expect("read timeout");
        let mut writer = stream.try_clone().expect("clone stub stream");
        let mut reader = BufReader::new(stream);
        writer
            .write_all(b"@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n")
            .expect("send greeting");
        for _ in 0..2 {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
        }
        for response in responses {
            writer
                .write_all(response.as_bytes())
                .expect("send response");
        }
        writer.flush().expect("flush responses");
    });
    (addr, handle)
}

/// Runs the shipped client against a fresh stub; `PORT` in an argument is
/// replaced by the stub's port.
fn run_client(args: &[&str], responses: Vec<String>) -> Output {
    let (addr, server) = spawn_stub_daemon(responses);
    let port = addr.port().to_string();
    let output = Command::new(oc_rsync_binary())
        .args(args.iter().map(|arg| arg.replace("PORT", &port)))
        .output()
        .expect("run client");
    server.join().expect("stub daemon thread");
    output
}

fn module_listing_responses() -> Vec<String> {
    vec![
        format!("{MOTD}\n"),
        format!("{MODULE_ROW}\n"),
        "@RSYNCD: EXIT\n".to_owned(),
    ]
}

fn refused_module_responses() -> Vec<String> {
    vec![
        format!("{MOTD}\n"),
        "@ERROR: Unknown module 'quietmod'\n".to_owned(),
    ]
}

/// Control: the stub really does produce a MOTD and a module row, so the quiet
/// test below cannot pass by the listing being empty.
#[test]
fn module_listing_prints_motd_and_rows_without_quiet() {
    let output = run_client(&["rsync://127.0.0.1:PORT/"], module_listing_responses());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(MOTD), "control: MOTD missing:\n{stdout}");
    assert!(
        stdout.contains("quiet-test comment"),
        "control: module row missing:\n{stdout}"
    );
}

#[test]
fn quiet_module_listing_prints_nothing() {
    for flags in [&["-q"][..], &["-qv"], &["-q", "-vv"]] {
        let mut args = flags.to_vec();
        args.push("rsync://127.0.0.1:PORT/");
        let output = run_client(&args, module_listing_responses());
        assert_eq!(output.status.code(), Some(0), "flags {flags:?}");
        assert!(
            output.stdout.is_empty(),
            "{flags:?}: upstream prints no FINFO line under -q, got:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// Control for the transfer path: without `-q` the MOTD reaches stdout.
#[test]
fn transfer_motd_is_printed_without_quiet() {
    let output = run_client(
        &["--list-only", "rsync://127.0.0.1:PORT/quietmod/"],
        refused_module_responses(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains(MOTD), "control: MOTD missing:\n{stdout}");
}

#[test]
fn quiet_transfer_suppresses_the_motd_but_not_the_error() {
    let output = run_client(
        &["-q", "--list-only", "rsync://127.0.0.1:PORT/quietmod/"],
        refused_module_responses(),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(MOTD),
        "upstream drops the MOTD under -q, got stdout:\n{stdout}"
    );
    assert!(
        stderr.contains("Unknown module"),
        "the @ERROR line is FERROR and must survive -q, got stderr:\n{stderr}"
    );
}
