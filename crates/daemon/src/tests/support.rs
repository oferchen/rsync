#[allow(unused_imports)]
use super::*;
use std::ffi::OsString;
#[cfg(unix)]
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::daemon::{
    HostPattern, ModuleDefinition, TEST_SECRETS_CANDIDATES, TEST_SECRETS_ENV,
    TestSecretsEnvOverride,
};
pub(super) use crate::test_env::{ENV_LOCK, EnvGuard};
use core::branding;

pub(super) const RSYNCD: &str = branding::daemon_program_name();
pub(super) const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

pub(super) fn base_module(name: &str) -> ModuleDefinition {
    ModuleDefinition {
        name: String::from(name),
        path: PathBuf::from("/srv/module"),
        comment: None,
        hosts_allow: Vec::new(),
        hosts_deny: Vec::new(),
        auth_users: Vec::new(),
        secrets_file: None,
        bandwidth_limit: None,
        bandwidth_limit_specified: false,
        bandwidth_burst: None,
        bandwidth_burst_specified: false,
        bandwidth_limit_configured: false,
        refuse_options: Vec::new(),
        read_only: true,
        write_only: false,
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
        incoming_chmod: None,
        outgoing_chmod: None,
        fake_super: false,
        munge_symlinks: None,
        max_verbosity: 1,
        ignore_errors: false,
        ignore_nonreadable: false,
        transfer_logging: false,
        log_format: Some("%o %h [%a] %m (%u) %f %l".to_owned()),
        dont_compress: None,
        pre_xfer_exec: None,
        post_xfer_exec: None,
        temp_dir: None,
        charset: None,
        forward_lookup: true,
        strict_modes: true,
    }
}

pub(super) fn module_with_host_patterns(allow: &[&str], deny: &[&str]) -> ModuleDefinition {
    ModuleDefinition {
        name: String::from("module"),
        path: PathBuf::from("/srv/module"),
        comment: None,
        hosts_allow: allow
            .iter()
            .map(|pattern| HostPattern::parse(pattern).expect("parse allow pattern"))
            .collect(),
        hosts_deny: deny
            .iter()
            .map(|pattern| HostPattern::parse(pattern).expect("parse deny pattern"))
            .collect(),
        auth_users: Vec::new(),
        secrets_file: None,
        bandwidth_limit: None,
        bandwidth_limit_specified: false,
        bandwidth_burst: None,
        bandwidth_burst_specified: false,
        bandwidth_limit_configured: false,
        refuse_options: Vec::new(),
        read_only: true,
        write_only: false,
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
        incoming_chmod: None,
        outgoing_chmod: None,
        fake_super: false,
        munge_symlinks: None,
        max_verbosity: 1,
        ignore_errors: false,
        ignore_nonreadable: false,
        transfer_logging: false,
        log_format: Some("%o %h [%a] %m (%u) %f %l".to_owned()),
        dont_compress: None,
        pre_xfer_exec: None,
        post_xfer_exec: None,
        temp_dir: None,
        charset: None,
        forward_lookup: true,
        strict_modes: true,
    }
}

pub(super) fn with_test_secrets_candidates<F, R>(candidates: Vec<PathBuf>, func: F) -> R
where
    F: FnOnce() -> R,
{
    TEST_SECRETS_CANDIDATES.with(|cell| {
        let previous = cell.replace(Some(candidates));
        let result = func();
        cell.replace(previous);
        result
    })
}

pub(super) fn with_test_secrets_env<F, R>(
    override_value: Option<TestSecretsEnvOverride>,
    func: F,
) -> R
where
    F: FnOnce() -> R,
{
    TEST_SECRETS_ENV.with(|cell| {
        let previous = cell.replace(override_value);
        let result = func();
        cell.replace(previous);
        result
    })
}

/// Allocates a free TCP port for daemon tests.
///
/// Binds to port 0 (OS-assigned ephemeral port) to guarantee no collision
/// at allocation time. The listener is kept alive and returned alongside the
/// port so the caller can hold it until the daemon is ready to bind,
/// minimizing the TOCTOU window between release and daemon bind.
pub(super) fn allocate_test_port() -> (u16, TcpListener) {
    let listener =
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral port for test");
    let port = listener.local_addr().expect("local addr").port();
    (port, listener)
}

pub(super) fn run_with_args<I, S>(args: I) -> (i32, Vec<u8>, Vec<u8>)
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run(args, &mut stdout, &mut stderr);
    (code, stdout, stderr)
}

#[cfg(unix)]
#[allow(dead_code)] // Used by disabled fallback tests
pub(super) fn write_executable_script(path: &Path, contents: &str) {
    fs::write(path, contents).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("set script permissions");
}

/// Spawn a daemon thread and connect, failing fast if the daemon exits early.
///
/// Returns the connected stream and the daemon thread handle.  If the daemon
/// thread finishes before a connection succeeds the actual daemon error is
/// included in the panic message instead of waiting for the full 30-second
/// timeout.
pub(super) fn start_daemon(
    config: crate::DaemonConfig,
    port: u16,
    held_listener: TcpListener,
) -> (
    TcpStream,
    thread::JoinHandle<Result<(), crate::DaemonError>>,
) {
    // Release the port reservation right before spawning the daemon thread
    // to minimize the TOCTOU window between release and the daemon's bind.
    drop(held_listener);
    let handle = thread::spawn(move || run_daemon(config));
    let stream = connect_to_daemon(port, Some(&handle));
    (stream, handle)
}

pub(super) fn connect_with_retries(port: u16) -> TcpStream {
    connect_to_daemon(port, None)
}

fn connect_to_daemon(
    port: u16,
    handle: Option<&thread::JoinHandle<Result<(), crate::DaemonError>>>,
) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_millis(500);
    // CI environments may have resource constraints causing slower daemon startup
    const TIMEOUT: Duration = Duration::from_secs(30);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
        // Fail fast if the daemon thread has already exited.
        if let Some(h) = handle {
            if h.is_finished() {
                panic!(
                    "daemon exited before accepting a connection on port {port}; \
                     check daemon startup logs for bind errors or config issues"
                );
            }
        }

        match TcpStream::connect_timeout(&target, backoff) {
            Ok(stream) => return stream,
            Err(error) => {
                if Instant::now() >= deadline {
                    panic!("failed to connect to daemon within timeout: {error}");
                }

                thread::sleep(backoff);
                backoff = (backoff.saturating_mul(2)).min(MAX_BACKOFF);
            }
        }
    }
}
