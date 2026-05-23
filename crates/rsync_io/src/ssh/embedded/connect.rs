//! High-level SSH connection establishment for the embedded transport.
//!
//! Provides `connect_and_exec` which handles the full lifecycle: DNS resolution,
//! TCP connect, authentication, channel open, and command execution. Returns
//! synchronous `Read`/`Write` handles suitable for the rsync protocol layer.

use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use super::auth::authenticate;
use super::config::SshConfig;
use super::error::SshError;
use super::handler::SshClientHandler;
use super::resolve::resolve_host;

/// Bounded budget for the SSH goodbye phase.
///
/// After the local side drops its writer half, the remote channel must
/// signal EOF (and the bridge task that drives `russh::Channel::wait()`
/// must drain and exit) within this window. The value is chosen as a
/// compromise between two failure modes:
///
/// - **Too small**: a slow but healthy network roundtrip during shutdown
///   would surface as a spurious goodbye-phase error.
/// - **Too large**: a real deadlock (the class fixed in PR #4154 / the
///   v0.6.1 200x SSH push regression) would again present as a multi-
///   minute hang instead of a typed error.
///
/// 30 seconds is comfortably above any healthy SSH channel teardown and
/// well below the operator-visible patience threshold, so a deadlock is
/// surfaced as `SshError::GoodbyePhaseTimeout` long before it shows up
/// as a wall-clock cliff.
pub const SSH_GOODBYE_TIMEOUT: Duration = Duration::from_secs(30);

/// Synchronous reader wrapping data received from the SSH channel.
///
/// Receives channel data chunks via a bounded sync channel and presents
/// them through `std::io::Read`. Partial reads from oversized chunks are
/// buffered internally.
pub struct ChannelReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    partial: Option<ReadBuffer>,
}

/// Partial buffer tracking for incomplete reads from channel messages.
struct ReadBuffer {
    data: Vec<u8>,
    offset: usize,
}

impl std::io::Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        // Drain any leftover data from a previous read.
        if let Some(ref mut rb) = self.partial {
            let remaining = &rb.data[rb.offset..];
            let n = std::cmp::min(remaining.len(), buf.len());
            buf[..n].copy_from_slice(&remaining[..n]);
            rb.offset += n;
            if rb.offset >= rb.data.len() {
                self.partial = None;
            }
            return Ok(n);
        }

        // Wait for next chunk from the channel.
        match self.rx.recv() {
            Ok(data) => {
                let n = std::cmp::min(data.len(), buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if n < data.len() {
                    self.partial = Some(ReadBuffer { data, offset: n });
                }
                Ok(n)
            }
            Err(_) => Ok(0), // Channel closed = EOF
        }
    }
}

impl ChannelReader {
    /// Waits for the remote SSH channel to signal EOF within
    /// [`SSH_GOODBYE_TIMEOUT`].
    ///
    /// Intended to be called after the local writer half has been dropped
    /// (the rsync protocol stream is complete) to enforce a bounded budget
    /// on the SSH session-shutdown handshake. Any chunks that arrive
    /// during the wait are discarded, because protocol-level reads are
    /// already finished by the caller.
    ///
    /// # Returns
    ///
    /// - `Ok(())` when the underlying bridge task exits cleanly (the
    ///   sender drop is observed as `RecvTimeoutError::Disconnected`).
    /// - `Err(SshError::GoodbyePhaseTimeout)` when the budget elapses
    ///   without the bridge having exited, signalling a deadlock on the
    ///   remote channel close.
    ///
    /// # Determinism
    ///
    /// The implementation tracks elapsed time against [`Instant::now`]
    /// rather than against the per-`recv_timeout` slice, so a sequence of
    /// short-lived chunks before the remote stalls still surfaces as a
    /// timeout error within the configured budget.
    pub fn wait_for_eof_with_timeout(&mut self, budget: Duration) -> Result<(), SshError> {
        let deadline = Instant::now() + budget;
        loop {
            let now = Instant::now();
            let remaining = if now >= deadline {
                Duration::ZERO
            } else {
                deadline - now
            };
            match self.rx.recv_timeout(remaining) {
                Ok(_chunk) => continue,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
                Err(RecvTimeoutError::Timeout) => {
                    return Err(SshError::GoodbyePhaseTimeout { elapsed: budget });
                }
            }
        }
    }
}

/// Synchronous writer forwarding data to the SSH channel.
///
/// Sends data to the async channel-forwarding task via a tokio mpsc channel.
/// `blocking_send` bridges the sync/async boundary.
pub struct ChannelWriter {
    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.tx
            .blocking_send(buf.to_vec())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Connects to a remote host via embedded SSH, authenticates, and executes
/// a remote command. Returns synchronous `Read`/`Write` handles for the
/// command's stdin/stdout.
///
/// This is the main entry point for the embedded SSH transport. The caller
/// provides a fully configured `SshConfig` (with any CLI overrides applied)
/// and the remote command string. The function:
///
/// 1. Creates a tokio current-thread runtime
/// 2. Resolves the host via DNS
/// 3. Establishes the SSH connection
/// 4. Authenticates (agent, key files, password - in OpenSSH order)
/// 5. Opens a session channel and executes the remote command
/// 6. Spawns a background task to bridge async channel I/O to sync handles
///
/// # Arguments
///
/// * `ssh_config` - SSH connection parameters
/// * `remote_command` - Shell command to execute on the remote host
/// * `stdin_data` - Optional data to send over stdin immediately after exec
///   (used for secluded-args delivery)
///
/// # Errors
///
/// Returns `SshError` for DNS resolution, connection, authentication,
/// or channel errors.
pub fn connect_and_exec(
    ssh_config: &SshConfig,
    remote_command: &str,
    stdin_data: Option<&[u8]>,
) -> Result<(ChannelReader, ChannelWriter), SshError> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SshError::Io(std::io::Error::other(format!("async runtime: {e}"))))?;

    rt.block_on(connect_and_exec_async(
        ssh_config,
        remote_command,
        stdin_data,
    ))
}

/// Async implementation of `connect_and_exec`.
async fn connect_and_exec_async(
    ssh_config: &SshConfig,
    remote_command: &str,
    stdin_data: Option<&[u8]>,
) -> Result<(ChannelReader, ChannelWriter), SshError> {
    let addrs = resolve_host(&ssh_config.host, ssh_config.port, ssh_config.ip_preference).await?;

    let addr = addrs
        .into_iter()
        .next()
        .ok_or_else(|| SshError::DnsResolution {
            host: ssh_config.host.clone(),
            preference: "any".to_owned(),
        })?;

    let client_config = Arc::new(russh::client::Config::default());

    let handler = SshClientHandler::new(
        ssh_config.host.clone(),
        ssh_config.port,
        ssh_config.strict_host_key_checking,
        ssh_config.known_hosts_file.clone(),
    );

    let mut handle = tokio::time::timeout(ssh_config.connect_timeout, async {
        russh::client::connect(client_config, addr, handler).await
    })
    .await
    .map_err(|_| SshError::Timeout {
        secs: ssh_config.connect_timeout.as_secs(),
    })??;

    authenticate(&mut handle, ssh_config).await?;

    let channel = handle
        .channel_open_session()
        .await
        .map_err(SshError::Connect)?;

    channel
        .exec(true, remote_command)
        .await
        .map_err(SshError::Connect)?;

    let (data_tx, data_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(64);
    let (write_tx, mut write_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);

    let channel_id = channel.id();
    let mut channel_for_read = channel;

    // Handle is not Clone in russh, so share via Arc<Mutex<_>> to allow the
    // async bridge task to write while the caller still holds a reference.
    let handle_shared = Arc::new(tokio::sync::Mutex::new(handle));
    let handle_for_write = handle_shared.clone();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = channel_for_read.wait() => {
                    match msg {
                        Some(russh::ChannelMsg::Data { data }) => {
                            if data_tx.send(data.to_vec()).is_err() {
                                break;
                            }
                        }
                        Some(russh::ChannelMsg::Eof) | None => {
                            break;
                        }
                        _ => continue,
                    }
                }
                write_data = write_rx.recv() => {
                    match write_data {
                        Some(data) => {
                            let h = handle_for_write.lock().await;
                            if h.data(channel_id, data).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    if let Some(data) = stdin_data {
        write_tx
            .send(data.to_vec())
            .await
            .map_err(|e| SshError::Io(std::io::Error::new(std::io::ErrorKind::BrokenPipe, e)))?;
    }

    Ok((
        ChannelReader {
            rx: data_rx,
            partial: None,
        },
        ChannelWriter { tx: write_tx },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn channel_reader_eof_on_closed_channel() {
        let (_, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let mut reader = ChannelReader { rx, partial: None };
        let mut buf = [0u8; 64];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn channel_reader_reads_full_chunk() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        tx.send(b"hello".to_vec()).unwrap();
        drop(tx);

        let mut reader = ChannelReader { rx, partial: None };
        let mut buf = [0u8; 64];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
    }

    #[test]
    fn channel_reader_partial_read_buffers_remainder() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        tx.send(b"hello world".to_vec()).unwrap();
        drop(tx);

        let mut reader = ChannelReader { rx, partial: None };

        let mut buf = [0u8; 5];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"hello");

        let mut buf2 = [0u8; 64];
        let n2 = reader.read(&mut buf2).unwrap();
        assert_eq!(&buf2[..n2], b" world");
    }

    /// A held-open bridge sender (the deadlock surface guarded by
    /// `SSH_GOODBYE_TIMEOUT`) surfaces as `GoodbyePhaseTimeout` within
    /// the configured budget, not as an unbounded hang.
    ///
    /// The budget is intentionally tiny (50 ms) so the test stays fast,
    /// and the upper bound is generous (2 s) so it stays deterministic on
    /// loaded CI runners. Using the budget as an upper bound rather than
    /// requiring wall-clock equality keeps the test from flaking under
    /// scheduling jitter.
    #[test]
    fn wait_for_eof_times_out_when_bridge_stalls() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let mut reader = ChannelReader { rx, partial: None };

        let budget = Duration::from_millis(50);
        let start = Instant::now();
        let result = reader.wait_for_eof_with_timeout(budget);
        let elapsed = start.elapsed();

        // Keep the sender alive past the call so the receiver can never
        // observe a Disconnected error - this is the simulated deadlock.
        drop(tx);

        match result {
            Err(SshError::GoodbyePhaseTimeout { elapsed: reported }) => {
                assert_eq!(reported, budget);
            }
            other => panic!("expected GoodbyePhaseTimeout, got {other:?}"),
        }

        assert!(
            elapsed >= budget,
            "returned before budget elapsed: {elapsed:?} < {budget:?}",
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "took longer than the upper bound: {elapsed:?}",
        );
    }

    /// Dropping the bridge sender models the healthy goodbye-phase path:
    /// the russh bridge task exits, the sender is dropped, and the
    /// receiver observes channel disconnect. The bounded wait helper
    /// must return `Ok(())` promptly.
    #[test]
    fn wait_for_eof_returns_ok_on_clean_drop() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(1);
        let mut reader = ChannelReader { rx, partial: None };
        drop(tx);

        let start = Instant::now();
        let result = reader.wait_for_eof_with_timeout(SSH_GOODBYE_TIMEOUT);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert!(
            elapsed < Duration::from_secs(1),
            "clean EOF should return promptly: {elapsed:?}",
        );
    }

    /// Chunks delivered before the remote stalls are discarded - the
    /// helper is only concerned with bounding the wait for channel
    /// disconnect, not with surfacing late protocol data. A budget that
    /// outlives the in-flight chunks still trips the timeout because the
    /// sender is never dropped.
    #[test]
    fn wait_for_eof_drains_pending_chunks_then_times_out() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
        tx.send(b"stale".to_vec()).unwrap();
        tx.send(b"goodbye-bytes".to_vec()).unwrap();
        let mut reader = ChannelReader { rx, partial: None };

        let budget = Duration::from_millis(75);
        let result = reader.wait_for_eof_with_timeout(budget);
        drop(tx);

        assert!(matches!(result, Err(SshError::GoodbyePhaseTimeout { .. })));
    }

    #[test]
    fn channel_writer_broken_pipe_on_closed_receiver() {
        // Create the channel outside any runtime - ChannelWriter::write uses
        // blocking_send which cannot be called from within a tokio runtime.
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        drop(rx);

        let mut writer = ChannelWriter { tx };
        let result = std::io::Write::write(&mut writer, b"data");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    }
}
