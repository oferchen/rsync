type WorkerResult = Result<(), (Option<SocketAddr>, io::Error)>;

/// Joins finished worker threads and propagates fatal errors.
///
/// Iterates through the worker list, joining any that have completed. This
/// prevents unbounded thread handle accumulation in long-running daemons.
fn reap_finished_workers(
    workers: &mut Vec<thread::JoinHandle<WorkerResult>>,
) -> Result<(), DaemonError> {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let handle = workers.remove(index);
            join_worker(handle)?;
        } else {
            index += 1;
        }
    }
    Ok(())
}

/// Waits for all remaining worker threads to complete.
fn drain_workers(workers: &mut Vec<thread::JoinHandle<WorkerResult>>) -> Result<(), DaemonError> {
    while let Some(handle) = workers.pop() {
        join_worker(handle)?;
    }
    Ok(())
}

/// Joins a single worker thread and maps its result to a `DaemonError`.
///
/// Normal connection closures (broken pipe, reset, aborted) are treated as
/// success. Panics that escape `catch_unwind` are logged and swallowed to
/// keep the daemon alive.
///
/// upstream: rsync forks per connection, so a crash only kills that
/// child process.
fn join_worker(handle: thread::JoinHandle<WorkerResult>) -> Result<(), DaemonError> {
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err((peer, error))) => {
            let kind = error.kind();
            if is_connection_closed_error(kind) {
                Ok(())
            } else {
                Err(stream_error(peer, "serve legacy handshake", error))
            }
        }
        Err(payload) => {
            let description = describe_panic_payload(payload);
            let error = io::Error::other(format!(
                "worker thread panicked (unwind escaped catch_unwind): {description}"
            ));
            eprintln!("{error} [daemon={}]", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

/// Extracts a human-readable message from a panic payload.
///
/// Handles the two common payload types (`String` and `&str`) and falls back
/// to a generic description for anything else.
fn describe_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "unknown panic payload".to_owned(),
        },
    }
}

/// Checks if an I/O error indicates a normal connection close.
const fn is_connection_closed_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}
