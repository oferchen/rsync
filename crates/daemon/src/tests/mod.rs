#![cfg(test)]

use super::*;
use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::num::{NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;
use tempfile::{NamedTempFile, tempdir};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

const RSYNCD: &str = branding::daemon_program_name();
const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

fn base_module(name: &str) -> ModuleDefinition {
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
        numeric_ids: false,
        uid: None,
        gid: None,
        timeout: None,
        listable: true,
        use_chroot: true,
        max_connections: None,
    }
}

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_test_secrets_candidates<F, R>(candidates: Vec<PathBuf>, func: F) -> R
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

fn with_test_secrets_env<F, R>(override_value: Option<TestSecretsEnvOverride>, func: F) -> R
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

fn allocate_test_port() -> u16 {
    const START: u16 = 40_000;
    const RANGE: u32 = 20_000;
    const STATE_SIZE: u64 = 4;

    let mut path = std::env::temp_dir();
    path.push("rsync-daemon-test-port.lock");

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

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

include!("part_01.rs");
include!("part_02.rs");
include!("part_03.rs");
include!("part_04.rs");
include!("part_05.rs");
include!("part_06.rs");
include!("part_07.rs");
include!("part_08.rs");
include!("part_09.rs");
