fn forward_client_to_child(
    mut upstream: TcpStream,
    mut child_stdin: ChildStdin,
    done: Arc<AtomicBool>,
) -> io::Result<u64> {
    upstream.set_read_timeout(Some(Duration::from_millis(200)))?;
    let mut forwarded = 0u64;
    let mut buffer = [0u8; 8192];

    loop {
        if done.load(Ordering::SeqCst) {
            break;
        }

        match upstream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                child_stdin.write_all(&buffer[..count])?;
                forwarded += u64::try_from(count).unwrap_or_default();
            }
            Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(ref err)
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.kind() == io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(err) => {
                if is_connection_closed_error(err.kind()) {
                    break;
                }

                return Err(err);
            }
        }
    }

    child_stdin.flush()?;
    Ok(forwarded)
}

#[derive(Clone)]
struct SessionDelegation {
    binary: OsString,
    args: Arc<[OsString]>,
}

impl SessionDelegation {
    fn new(binary: OsString, args: Vec<OsString>) -> Self {
        Self {
            binary,
            args: Arc::from(args.into_boxed_slice()),
        }
    }

    fn binary(&self) -> &OsString {
        &self.binary
    }

    fn args(&self) -> &[OsString] {
        &self.args
    }
}

fn delegate_binary_session(
    stream: TcpStream,
    delegation: &SessionDelegation,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    let binary = delegation.binary();
    if let Some(log) = log_sink {
        let text = format!(
            "delegating binary session to '{}'",
            Path::new(binary).display()
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let mut command = ProcessCommand::new(binary);
    command.arg("--daemon");
    command.arg("--no-detach");
    command.args(delegation.args());
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::inherit());

    let mut child = command.spawn()?;
    let child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "fallback stdin unavailable"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "fallback stdout unavailable"))?;

    let upstream = stream.try_clone()?;
    let downstream = stream.try_clone()?;
    let control_stream = stream;
    let completion = Arc::new(AtomicBool::new(false));
    let reader_completion = Arc::clone(&completion);
    let writer_completion = Arc::clone(&completion);

    let reader =
        thread::spawn(move || forward_client_to_child(upstream, child_stdin, reader_completion));

    let writer = thread::spawn(move || {
        let mut downstream = downstream;
        let result = io::copy(&mut child_stdout, &mut downstream);
        writer_completion.store(true, Ordering::SeqCst);
        result
    });

    let status = child.wait()?;
    completion.store(true, Ordering::SeqCst);

    let write_bytes = writer
        .join()
        .map_err(|_| io::Error::other("failed to join writer thread"))??;

    #[allow(unused_must_use)]
    {
        use std::net::Shutdown;
        control_stream.shutdown(Shutdown::Both);
    }

    let read_bytes = reader
        .join()
        .map_err(|_| io::Error::other("failed to join reader thread"))??;

    if let Some(log) = log_sink {
        let text =
            format!("forwarded {read_bytes} bytes to fallback and received {write_bytes} bytes");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    if !status.success()
        && let Some(log) = log_sink {
            let text = format!(
                "fallback daemon '{}' exited with status {}",
                Path::new(binary).display(),
                status
            );
            let message = rsync_warning!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }

    Ok(())
}

pub(crate) fn legacy_daemon_greeting() -> String {
    let mut greeting =
        format_legacy_daemon_message(LegacyDaemonMessage::Version(ProtocolVersion::NEWEST));
    debug_assert!(greeting.ends_with('\n'));
    greeting.pop();

    for digest in SUPPORTED_DAEMON_DIGESTS {
        greeting.push(' ');
        greeting.push_str(digest.name());
    }

    greeting.push('\n');
    greeting
}

pub(crate) fn read_trimmed_line<R: BufRead>(reader: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    let bytes = reader.read_line(&mut line)?;

    if bytes == 0 {
        return Ok(None);
    }

    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }

    Ok(Some(line))
}

fn advertise_capabilities(
    stream: &mut TcpStream,
    modules: &[ModuleRuntime],
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    for payload in advertised_capability_lines(modules) {
        let message = messages.render(LegacyDaemonMessage::Capabilities {
            flags: payload.as_str(),
        });
        stream.write_all(message.as_bytes())?;
    }

    if modules.is_empty() {
        Ok(())
    } else {
        stream.flush()
    }
}

pub(crate) fn advertised_capability_lines(modules: &[ModuleRuntime]) -> Vec<String> {
    if modules.is_empty() {
        return Vec::new();
    }

    let mut features = Vec::with_capacity(2);
    features.push(String::from("modules"));

    if modules
        .iter()
        .any(|module| module.requires_authentication())
    {
        features.push(String::from("authlist"));
    }

    vec![features.join(" ")]
}

