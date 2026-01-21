use super::*;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
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
use fs2::FileExt;

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

pub(super) fn allocate_test_port() -> u16 {
    const START: u16 = 40_000;
    const RANGE: u32 = 20_000;
    const STATE_SIZE: u64 = 4;

    let mut path = std::env::temp_dir();
    path.push("daemon-test-port.lock");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .expect("open port allocator state");

    file.lock_exclusive().expect("lock port allocator state");
    file.seek(SeekFrom::Start(0))
        .expect("rewind port allocator state");

    let mut counter_bytes = [0u8; STATE_SIZE as usize];
    let read = file
        .read(&mut counter_bytes)
        .expect("read port allocator state");
    let mut counter = if read == counter_bytes.len() {
        u32::from_le_bytes(counter_bytes)
    } else {
        0
    };

    for _ in 0..RANGE {
        let offset = (counter % RANGE) as u16;
        counter = counter.wrapping_add(1);

        file.seek(SeekFrom::Start(0))
            .expect("rewind port allocator state");
        file.write_all(&counter.to_le_bytes())
            .expect("persist port allocator state");
        file.set_len(STATE_SIZE)
            .expect("truncate port allocator state");
        file.flush().expect("flush port allocator state");

        let candidate = START + offset;
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::LOCALHOST, candidate)) {
            drop(listener);
            return candidate;
        }
    }

    panic!("failed to allocate a free test port");
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

pub(super) fn connect_with_retries(port: u16) -> TcpStream {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_millis(500);
    // CI environments may have resource constraints causing slower daemon startup
    const TIMEOUT: Duration = Duration::from_secs(30);

    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + TIMEOUT;
    let mut backoff = INITIAL_BACKOFF;

    loop {
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
