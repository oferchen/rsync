//! Regression test for daemon connection lifecycle through the wired
//! ConnectionState FSM (FSW-8).
//!
//! Each test spawns a real daemon on an ephemeral port, drives a TCP
//! client through a specific protocol path, and verifies that the daemon
//! completes without panic or protocol error. The FSM transitions happen
//! inside the daemon's session handler (session_runtime.rs, request.rs,
//! transfer.rs); a successful handshake round-trip proves the FSM allowed
//! every transition that the protocol path required.
//!
//! Tested paths:
//!
//! - **list**: Greeting -> ModuleSelect -> Closing (via `#list`)
//! - **unknown module**: Greeting -> ModuleSelect -> Closing (via @ERROR)
//! - **early exit**: Greeting -> Closing (client sends @RSYNCD: EXIT)
//! - **immediate disconnect**: Greeting -> Closing (TCP RST)
//! - **multiple sequential connections**: each traverses the full lifecycle
//!   independently, proving no leaked state between sessions

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use daemon::{DaemonConfig, run_daemon};
use platform::signal::SignalFlags;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(5);

fn allocate_listener() -> (u16, TcpListener) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
    let port = listener.local_addr().expect("local addr").port();
    (port, listener)
}

fn spawn_daemon(
    listener: TcpListener,
    port: u16,
) -> (
    thread::JoinHandle<Result<(), daemon::DaemonError>>,
    SignalFlags,
) {
    let flags = SignalFlags::new();
    let config = DaemonConfig::builder()
        .disable_default_paths()
        .arguments([
            "--no-detach".to_string(),
            "--port".to_string(),
            port.to_string(),
        ])
        .pre_bound_listener(listener)
        .signal_flags(flags.clone())
        .build();
    let handle = thread::spawn(move || run_daemon(config));
    (handle, flags)
}

fn connect_with_timeout(port: u16) -> TcpStream {
    let addr = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let stream = loop {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                thread::sleep(Duration::from_millis(20));
                continue;
            }
            Err(e) => panic!("unexpected connect error: {e}"),
        }
    };
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .expect("set_read_timeout");
    stream
        .set_write_timeout(Some(READ_TIMEOUT))
        .expect("set_write_timeout");
    stream
}

fn read_greeting(reader: &mut BufReader<TcpStream>) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).expect("read greeting");
    assert!(
        line.starts_with("@RSYNCD:"),
        "expected @RSYNCD greeting, got: {line:?}"
    );
    line
}

fn send_version(stream: &mut TcpStream) {
    stream
        .write_all(b"@RSYNCD: 32.0\n")
        .expect("send version");
    stream.flush().expect("flush version");
}

/// Greeting -> ModuleSelect -> Closing via `#list`.
///
/// The daemon receives the version, transitions Greeting -> ModuleSelect,
/// then receives `#list`, sends the module listing, writes @RSYNCD: EXIT,
/// and transitions ModuleSelect -> Closing.
#[test]
fn lifecycle_list_modules() {
    let (port, listener) = allocate_listener();
    let (handle, flags) = spawn_daemon(listener, port);

    let stream = connect_with_timeout(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;

    read_greeting(&mut reader);
    send_version(&mut writer);

    writer.write_all(b"#list\n").expect("send #list");
    writer.flush().expect("flush #list");

    let mut saw_exit = false;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Err(_) => break,
            Ok(_) => {}
        }
        if line.trim() == "@RSYNCD: EXIT" {
            saw_exit = true;
            break;
        }
    }
    assert!(saw_exit, "daemon should send @RSYNCD: EXIT after #list");

    drop(writer);
    drop(reader);

    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
}

/// Greeting -> ModuleSelect -> Closing via unknown module.
///
/// The daemon receives the version (Greeting -> ModuleSelect), then the
/// client requests a non-existent module. The daemon sends @ERROR and
/// @RSYNCD: EXIT, transitioning ModuleSelect -> Closing.
#[test]
fn lifecycle_unknown_module() {
    let (port, listener) = allocate_listener();
    let (handle, flags) = spawn_daemon(listener, port);

    let stream = connect_with_timeout(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;

    read_greeting(&mut reader);
    send_version(&mut writer);

    writer
        .write_all(b"nonexistent_module_fsw8\n")
        .expect("send module");
    writer.flush().expect("flush module");

    let mut saw_error = false;
    let mut saw_exit = false;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Err(_) => break,
            Ok(_) => {}
        }
        if line.starts_with("@ERROR") {
            saw_error = true;
        }
        if line.trim() == "@RSYNCD: EXIT" {
            saw_exit = true;
            break;
        }
    }
    assert!(saw_error, "daemon should send @ERROR for unknown module");
    assert!(saw_exit, "daemon should send @RSYNCD: EXIT after error");

    drop(writer);
    drop(reader);

    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should exit cleanly: {result:?}");
}

/// Greeting -> Closing via client @RSYNCD: EXIT.
///
/// The client sends @RSYNCD: EXIT immediately after the version exchange.
/// The daemon transitions Greeting -> ModuleSelect (on version), then
/// ModuleSelect -> Closing (on EXIT).
#[test]
fn lifecycle_early_exit_after_version() {
    let (port, listener) = allocate_listener();
    let (handle, flags) = spawn_daemon(listener, port);

    let stream = connect_with_timeout(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;

    read_greeting(&mut reader);
    send_version(&mut writer);

    writer
        .write_all(b"@RSYNCD: EXIT\n")
        .expect("send EXIT");
    writer.flush().expect("flush EXIT");

    drop(writer);
    drop(reader);

    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should exit cleanly on early EXIT: {result:?}");
}

/// Greeting -> Closing via immediate TCP disconnect.
///
/// The client connects, reads the greeting, then drops the connection
/// without sending any data. The daemon sees EOF and transitions to
/// Closing without panic.
#[test]
fn lifecycle_immediate_disconnect() {
    let (port, listener) = allocate_listener();
    let (handle, flags) = spawn_daemon(listener, port);

    let stream = connect_with_timeout(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));

    read_greeting(&mut reader);

    // Drop both ends without sending anything.
    drop(reader);
    drop(stream);

    // Give the daemon a moment to process the EOF.
    thread::sleep(Duration::from_millis(100));

    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread");
    assert!(result.is_ok(), "daemon should handle immediate disconnect: {result:?}");
}

/// Multiple sequential connections each traverse the full lifecycle.
///
/// Proves that the daemon does not leak FSM state between connections.
/// Each connection exercises Greeting -> ModuleSelect -> Closing (via
/// unknown module).
#[test]
fn lifecycle_multiple_sequential_connections() {
    let (port, listener) = allocate_listener();
    let (handle, flags) = spawn_daemon(listener, port);

    for i in 0..5 {
        let stream = connect_with_timeout(port);
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut writer = stream;

        read_greeting(&mut reader);
        send_version(&mut writer);

        let module = format!("no_such_module_{i}\n");
        writer.write_all(module.as_bytes()).expect("send module");
        writer.flush().expect("flush module");

        let mut saw_exit = false;
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Err(_) => break,
                Ok(_) => {}
            }
            if line.trim() == "@RSYNCD: EXIT" {
                saw_exit = true;
                break;
            }
        }
        assert!(
            saw_exit,
            "connection {i}: daemon should send @RSYNCD: EXIT"
        );

        drop(writer);
        drop(reader);
    }

    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread");
    assert!(
        result.is_ok(),
        "daemon should handle multiple sequential connections: {result:?}"
    );
}

/// Greeting -> ModuleSelect -> Closing: version only, then disconnect.
///
/// The client sends its version (triggering Greeting -> ModuleSelect)
/// then disconnects without sending a module name. The daemon reads
/// EOF on the module-name line, sends an error response, and
/// transitions to Closing.
#[test]
fn lifecycle_version_then_disconnect() {
    let (port, listener) = allocate_listener();
    let (handle, flags) = spawn_daemon(listener, port);

    let stream = connect_with_timeout(port);
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut writer = stream;

    read_greeting(&mut reader);
    send_version(&mut writer);

    // Drop writer side to send FIN, triggering EOF on daemon read.
    drop(writer);

    // Drain any response the daemon sends before closing.
    let mut buf = [0u8; 256];
    loop {
        match reader.get_mut().read(&mut buf) {
            Ok(0) => break,
            Err(_) => break,
            Ok(_) => {}
        }
    }
    drop(reader);

    thread::sleep(Duration::from_millis(100));

    flags.shutdown.store(true, Ordering::Relaxed);
    let result = handle.join().expect("daemon thread");
    assert!(
        result.is_ok(),
        "daemon should handle version-then-disconnect: {result:?}"
    );
}
