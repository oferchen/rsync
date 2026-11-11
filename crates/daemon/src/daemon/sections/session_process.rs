/// What a Windows daemon needs to start one session process per connection.
///
/// upstream: socket.c:755-780 - `start_accept_loop()` forks a child for every
/// accepted connection. Windows has no `fork`, so the daemon starts
/// `PROGRAM --daemon-session` instead and hands it the connection.
#[cfg(windows)]
struct SessionSpawn {
    /// The program started for each connection.
    executable: PathBuf,
    /// The daemon's own arguments, re-parsed by the child.
    arguments: Vec<Vec<u16>>,
    /// Whether the daemon consults the default configuration paths.
    load_default_paths: bool,
}

#[cfg(windows)]
impl SessionSpawn {
    /// The spawn plan for `config`, or `None` when sessions run on threads.
    fn for_config(config: &DaemonConfig) -> Option<Self> {
        use std::os::windows::ffi::OsStrExt;
        let executable = config.session_executable()?.to_path_buf();
        Some(Self {
            executable,
            arguments: config
                .arguments()
                .iter()
                .map(|argument| argument.encode_wide().collect())
                .collect(),
            load_default_paths: config.load_default_paths(),
        })
    }

    /// Starts a session process for `stream` and hands the connection to it.
    ///
    /// The child is created suspended and only resumed once the socket has
    /// been duplicated for it, so it never runs without its connection. The
    /// hand-off frame is written after the resume: the child is then reading
    /// its pipe, so a frame larger than the pipe buffer cannot deadlock.
    fn start(&self, stream: &DaemonStream) -> io::Result<platform::session_process::SessionChild> {
        use std::os::windows::io::AsHandle;

        let socket = stream.tcp_stream().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "only a TCP connection can be handed to a session process",
            )
        })?;
        let (reader, mut writer) = io::pipe()?;
        let stdout = std::fs::OpenOptions::new().write(true).open("NUL")?;
        // A duplicate, so marking it inheritable leaves the daemon's own
        // standard error untouched.
        let stderr = match io::stderr().as_handle().try_clone_to_owned() {
            Ok(handle) => handle,
            Err(_) => stdout.as_handle().try_clone_to_owned()?,
        };
        let stdio = platform::session_process::ChildStdio {
            stdin: reader.as_handle(),
            stdout: stdout.as_handle(),
            stderr: stderr.as_handle(),
        };
        let child = platform::session_process::spawn_suspended(&self.executable, &stdio)?;
        drop((reader, stdout, stderr));

        let handoff = crate::connection_handoff::ConnectionHandoff {
            socket_info: platform::session_process::duplicate_socket(socket, child.pid())?,
            load_default_paths: self.load_default_paths,
            arguments: self.arguments.clone(),
        };
        let mut child = child.resume()?;
        if let Err(error) = handoff.write_to(&mut writer) {
            // The child exits once its pipe closes; reap it so the failure
            // does not leave a process behind.
            drop(writer);
            let _ = child.wait();
            return Err(error);
        }
        Ok(child)
    }
}

/// Serves the one connection a Windows daemon handed to this process.
///
/// This is the body of `oc-rsync --daemon-session`: it reads the hand-off
/// frame from standard input, re-parses the daemon's arguments (re-reading
/// the configuration, as upstream's `start_daemon()` does for every connection
/// at clientserver.c:1434), rebuilds the socket and serves the session.
///
/// # Errors
///
/// Returns a `DaemonError` when the hand-off is missing or malformed, when the
/// daemon arguments no longer parse, or when the session fails.
#[cfg(windows)]
pub fn run_daemon_session_child(brand: Brand) -> Result<(), DaemonError> {
    use std::os::windows::ffi::OsStringExt;

    use std::os::windows::io::AsRawSocket;

    let handoff = read_connection_handoff().map_err(child_not_created)?;
    let arguments: Vec<OsString> = handoff
        .arguments
        .iter()
        .map(|units| OsString::from_wide(units))
        .collect();
    let options = RuntimeOptions::parse_with_brand(&arguments, brand, handoff.load_default_paths)?;
    apply_verbosity(options.verbosity());
    let socket =
        platform::session_process::adopt_socket(&handoff.socket_info).map_err(child_not_created)?;
    let fd = socket.as_raw_socket();
    serve_socket_session(options, socket, fd, "daemon session failed")
}

/// A session child that cannot take over its connection is, to 3.5.1, a child
/// that was never created: under Cygwin `fork()` carries the socket into the
/// child itself, and its failure is reported as below.
///
/// upstream: socket.c:773-779 `rsyserr(FERROR, errno, "could not create child
/// server process")`.
#[cfg(windows)]
fn child_not_created(error: io::Error) -> DaemonError {
    let code = ExitCode::Ipc;
    DaemonError::with_code(
        code,
        rsync_error!(
            code.as_i32(),
            format!("could not create child server process: {error}")
        )
        .with_role(Role::Daemon),
    )
}

#[cfg(windows)]
fn read_connection_handoff() -> io::Result<crate::connection_handoff::ConnectionHandoff> {
    use std::os::windows::io::AsHandle;
    let stdin = io::stdin().as_handle().try_clone_to_owned()?;
    let mut pipe = platform::session_process::adopt_handoff_pipe(stdin)?;
    crate::connection_handoff::ConnectionHandoff::read_from(&mut pipe)
}
