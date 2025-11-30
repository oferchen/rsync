use super::*;
use core::branding::rust_version;

pub(super) const RSYNC: &str = branding::client_program_name();
pub(super) const OC_RSYNC: &str = branding::oc_client_program_name();
pub(super) const RSYNCD: &str = branding::daemon_program_name();
pub(super) const OC_RSYNC_D: &str = branding::oc_daemon_program_name();

pub(super) const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";

pub(super) fn spawn_stub_daemon(
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    spawn_stub_daemon_with_protocol(responses, "32.0")
}

pub(super) fn spawn_stub_daemon_with_protocol(
    responses: Vec<&'static str>,
    expected_protocol: &'static str,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_connection(stream, responses, expected_protocol);
        }
    });

    (addr, handle)
}

pub(super) fn spawn_auth_stub_daemon(
    challenge: &'static str,
    expected_credentials: String,
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind auth daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_auth_connection(stream, challenge, &expected_credentials, &responses, "32.0");
        }
    });

    (addr, handle)
}

fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>, expected_protocol: &str) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    stream
        .write_all(LEGACY_DAEMON_GREETING.as_bytes())
        .expect("write greeting");
    stream.flush().expect("flush greeting");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read client greeting");
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let expected_prefix = ["@RSYNCD: ", expected_protocol].concat();
    assert!(
        trimmed.starts_with(&expected_prefix),
        "client greeting {trimmed:?} did not begin with {expected_prefix:?}"
    );

    line.clear();
    reader.read_line(&mut line).expect("read request");
    assert_eq!(line, "#list\n");

    for response in responses {
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .expect("write response");
    }
    reader.get_mut().flush().expect("flush response");
}

fn handle_auth_connection(
    mut stream: TcpStream,
    challenge: &'static str,
    expected_credentials: &str,
    responses: &[&'static str],
    expected_protocol: &str,
) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .expect("set write timeout");

    stream
        .write_all(LEGACY_DAEMON_GREETING.as_bytes())
        .expect("write greeting");
    stream.flush().expect("flush greeting");

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read client greeting");
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let expected_prefix = ["@RSYNCD: ", expected_protocol].concat();
    assert!(
        trimmed.starts_with(&expected_prefix),
        "client greeting {trimmed:?} did not begin with {expected_prefix:?}"
    );

    line.clear();
    reader.read_line(&mut line).expect("read request");
    assert_eq!(line, "#list\n");

    reader
        .get_mut()
        .write_all(format!("@RSYNCD: AUTHREQD {challenge}\n").as_bytes())
        .expect("write challenge");
    reader.get_mut().flush().expect("flush challenge");

    line.clear();
    reader.read_line(&mut line).expect("read credentials");
    let received = line.trim_end_matches(['\n', '\r']);
    assert_eq!(received, expected_credentials);

    for response in responses {
        reader
            .get_mut()
            .write_all(response.as_bytes())
            .expect("write response");
    }
    reader.get_mut().flush().expect("flush response");
}

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
pub(super) fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    use std::convert::TryInto;

    let bits: u16 = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let mode = Mode::from_bits_truncate(bits.into());
    mknodat(CWD, path, FileType::Fifo, mode, makedev(0, 0)).map_err(io::Error::from)
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
pub(super) fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use std::convert::TryInto;

    let bits: libc::mode_t = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    apple_fs::mkfifo(path, bits)
}

pub(super) fn assert_contains_client_trailer(rendered: &str) {
    let expected = format!("[client={}]", rust_version());
    assert!(
        rendered.contains(&expected),
        "expected message to contain {expected:?}, got {rendered:?}"
    );
}

#[allow(dead_code)]
pub(super) fn assert_contains_server_trailer(rendered: &str) {
    let expected = format!("[server={}]", rust_version());
    assert!(
        rendered.contains(&expected),
        "expected message to contain {expected:?}, got {rendered:?}"
    );
}

pub(super) static ENV_LOCK: Mutex<()> = Mutex::new(());

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

pub(super) struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

#[allow(unsafe_code)]
impl EnvGuard {
    pub(super) fn set(key: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    /// Temporarily removes the environment variable for the duration of the guard.
    pub(super) fn remove(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

#[allow(unsafe_code)]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            unsafe {
                std::env::set_var(self.key, value);
            }
        } else {
            unsafe {
                std::env::remove_var(self.key);
            }
        }
    }
}

pub(super) fn clear_rsync_rsh() -> EnvGuard {
    EnvGuard::set("RSYNC_RSH", OsStr::new(""))
}

#[cfg(unix)]
pub(super) fn write_executable_script(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write script");
    let mut permissions = std::fs::metadata(path)
        .expect("script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("set script permissions");
}
