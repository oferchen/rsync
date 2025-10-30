use super::prelude::*;

pub(super) const LEGACY_DAEMON_GREETING: &str = "@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n";


#[cfg(unix)]
const CAPTURE_PASSWORD_SCRIPT: &str = r#"#!/bin/sh
set -eu
OUTPUT=""
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  OUTPUT="${arg#CAPTURE=}"
  ;;
  esac
done
: "${OUTPUT:?}"
cat > "$OUTPUT"
"#;


#[cfg(unix)]
const CAPTURE_ARGS_SCRIPT: &str = r#"#!/bin/sh
set -eu
OUTPUT=""
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  OUTPUT="${arg#CAPTURE=}"
  ;;
  esac
done
: "${OUTPUT:?}"
: > "$OUTPUT"
for arg in "$@"; do
  case "$arg" in
CAPTURE=*)
  ;;
*)
  printf '%s\n' "$arg" >> "$OUTPUT"
  ;;
  esac
done
"#;


#[cfg(unix)]
pub(super) fn capture_password_script() -> String {
    CAPTURE_PASSWORD_SCRIPT.to_string()
}


#[cfg(unix)]
pub(super) fn capture_args_script() -> String {
    CAPTURE_ARGS_SCRIPT.to_string()
}


#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;


#[cfg(unix)]
const FALLBACK_SCRIPT: &str = r#"#!/bin/sh
set -eu

while [ "$#" -gt 0 ]; do
  case "$1" in
--files-from)
  FILE="$2"
  cat "$FILE"
  shift 2
  ;;
--from0)
  shift
  ;;
*)
  shift
  ;;
  esac
done

printf 'fallback stdout\n'
printf 'fallback stderr\n' >&2
exit 42
"#;


static ENV_GUARD: OnceLock<Mutex<()>> = OnceLock::new();


pub(super) fn env_lock() -> &'static Mutex<()> {
    ENV_GUARD.get_or_init(|| Mutex::new(()))
}


#[cfg(unix)]
pub(super) fn write_fallback_script(dir: &Path) -> PathBuf {
    let path = dir.join("fallback.sh");
    fs::write(&path, FALLBACK_SCRIPT).expect("script written");
    let metadata = fs::metadata(&path).expect("script metadata");
    let mut permissions = metadata.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("script permissions set");
    path
}


pub(super) fn baseline_fallback_args() -> RemoteFallbackArgs {
    RemoteFallbackArgs {
        dry_run: false,
        list_only: false,
        remote_shell: None,
        remote_options: Vec::new(),
        connect_program: None,
        port: None,
        bind_address: None,
        protect_args: None,
        human_readable: None,
        address_mode: AddressMode::Default,
        archive: false,
        delete: false,
        delete_mode: DeleteMode::Disabled,
        delete_excluded: false,
        max_delete: None,
        min_size: None,
        max_size: None,
        checksum: false,
        checksum_choice: None,
        checksum_seed: None,
        size_only: false,
        ignore_existing: false,
        ignore_missing_args: false,
        update: false,
        modify_window: None,
        compress: false,
        compress_disabled: false,
        compress_level: None,
        skip_compress: None,
        chown: None,
        owner: None,
        group: None,
        chmod: Vec::new(),
        perms: None,
        super_mode: None,
        times: None,
        omit_dir_times: None,
        omit_link_times: None,
        numeric_ids: None,
        hard_links: None,
        copy_links: None,
        copy_dirlinks: false,
        copy_unsafe_links: None,
        keep_dirlinks: None,
        safe_links: false,
        sparse: None,
        devices: None,
        specials: None,
        relative: None,
        one_file_system: None,
        implied_dirs: None,
        mkpath: false,
        prune_empty_dirs: None,
        verbosity: 0,
        progress: false,
        stats: false,
        itemize_changes: false,
        partial: false,
        preallocate: false,
        delay_updates: false,
        partial_dir: None,
        temp_directory: None,
        backup: false,
        backup_dir: None,
        backup_suffix: None,
        link_dests: Vec::new(),
        remove_source_files: false,
        append: None,
        append_verify: false,
        inplace: None,
        msgs_to_stderr: false,
        whole_file: None,
        bwlimit: None,
        excludes: Vec::new(),
        includes: Vec::new(),
        exclude_from: Vec::new(),
        include_from: Vec::new(),
        filters: Vec::new(),
        rsync_filter_shortcuts: 0,
        compare_destinations: Vec::new(),
        copy_destinations: Vec::new(),
        link_destinations: Vec::new(),
        cvs_exclude: false,
        info_flags: Vec::new(),
        debug_flags: Vec::new(),
        files_from_used: false,
        file_list_entries: Vec::new(),
        from0: false,
        password_file: None,
        daemon_password: None,
        protocol: None,
        timeout: TransferTimeout::Default,
        connect_timeout: TransferTimeout::Default,
        out_format: None,
        no_motd: false,
        fallback_binary: None,
        rsync_path: None,
        remainder: Vec::new(),
        #[cfg(feature = "acl")]
        acls: None,
        #[cfg(feature = "xattr")]
        xattrs: None,
    }
}


#[cfg(unix)]
pub(super) struct FailingWriter;

#[cfg(unix)]
impl Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("forced failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("forced failure"))
    }
}


pub(super) struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}


impl EnvGuard {
    pub(super) fn set(key: &'static str, value: &str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    pub(super) fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::set_var(key, value);
        }
        Self { key, previous }
    }

    pub(super) fn remove(key: &'static str) -> Self {
        let previous = env::var_os(key);
        #[allow(unsafe_code)]
        unsafe {
            env::remove_var(key);
        }
        Self { key, previous }
    }
}


impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(self.key, value);
            }
        } else {
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(self.key);
            }
        }
    }
}


pub(super) const DEFAULT_PROXY_STATUS_LINE: &str = "HTTP/1.0 200 Connection established";

pub(super) const LOWERCASE_PROXY_STATUS_LINE: &str = "http/1.1 200 Connection Established";


pub(super) fn spawn_stub_proxy(
    target: std::net::SocketAddr,
    expected_header: Option<&'static str>,
    status_line: &'static str,
) -> (
    std::net::SocketAddr,
    mpsc::Receiver<String>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
    let addr = listener.local_addr().expect("proxy addr");
    let (tx, rx) = mpsc::channel();

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let mut reader = BufReader::new(stream);
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).expect("read request line") == 0 {
                    break;
                }
                captured.push_str(&line);
                if line == "\r\n" || line == "\n" {
                    break;
                }
            }

            if let Some(expected) = expected_header {
                assert!(captured.contains(expected), "missing proxy header");
            }

            tx.send(captured).expect("send captured request");

            let mut client_stream = reader.into_inner();
            let mut server_stream = TcpStream::connect(target).expect("connect daemon");
            client_stream
                .write_all(status_line.as_bytes())
                .expect("write proxy response");
            client_stream
                .write_all(b"\r\n\r\n")
                .expect("terminate proxy status");

            let mut client_clone = client_stream.try_clone().expect("clone client");
            let mut server_clone = server_stream.try_clone().expect("clone server");

            let forward = thread::spawn(move || {
                let _ = io::copy(&mut client_clone, &mut server_stream);
            });
            let backward = thread::spawn(move || {
                let _ = io::copy(&mut server_clone, &mut client_stream);
            });

            let _ = forward.join();
            let _ = backward.join();
        }
    });

    (addr, rx, handle)
}


pub(super) fn spawn_stub_daemon(
    responses: Vec<&'static str>,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub daemon");
    let addr = listener.local_addr().expect("local addr");

    let handle = thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            handle_connection(stream, responses);
        }
    });

    (addr, handle)
}


pub(super) fn handle_connection(mut stream: TcpStream, responses: Vec<&'static str>) {
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
    assert_eq!(line, LEGACY_DAEMON_GREETING);

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

    let stream = reader.into_inner();
    let _ = stream.shutdown(Shutdown::Both);
}

