//! The four network transcript cells: push and pull over a remote shell and
//! over the daemon protocol.
//!
//! [`crate::transcript`] captures a remote-shell transfer through the
//! `capture-rsh` trampoline. A daemon transfer has no trampoline to hook, so
//! [`capture_cell`] puts a loopback TCP relay between the client and an
//! `oc-rsync --daemon` and tees both directions instead. Every cell applies the
//! same normalization set ([`crate::transcript::normalized_client`]) and the
//! same backdated fixture ([`build_fixture`]), so one binary produces
//! byte-identical transcripts run after run, and two builds can be compared
//! cell by cell (the `transcript-ab` tool).
//!
//! The daemon cells cover the paths the remote-shell cells cannot: the
//! `@RSYNCD:` greeting and module exchange, the daemon's unidirectional
//! negotiation, and the daemon's own socket handling, where the client and
//! server share one full-duplex descriptor (upstream: clientserver.c
//! start_daemon(), io.c perform_io() polling one fd for both directions).

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime};

use crate::transcript::{TranscriptError, TranscriptRecorder, normalized_client};
use crate::{CLIENT_IO_TIMEOUT, DaemonProcess, WireTranscript};

/// Name of the module every daemon cell transfers through.
const MODULE: &str = "cell";

/// Fixed mtime for every fixture entry (2023-11-14T22:13:20Z).
///
/// The file list carries each entry's mtime, and the quick check compares
/// size and mtime, so an unpinned mtime moves wire bytes between fixture
/// builds.
const FIXED_MTIME_SECS: u64 = 1_700_000_000;

/// One transfer shape whose wire bytes a transcript gate compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkCell {
    /// `oc-rsync SRC/ host:DEST` through `capture-rsh`; the client sends.
    RshPush,
    /// `oc-rsync host:SRC/ DEST` through `capture-rsh`; the client receives.
    RshPull,
    /// `oc-rsync SRC/ rsync://host/module/`; the daemon receives.
    DaemonPush,
    /// `oc-rsync rsync://host/module/ DEST`; the daemon sends.
    DaemonPull,
}

impl NetworkCell {
    /// Every cell, in report order.
    pub const ALL: [Self; 4] = [
        Self::RshPush,
        Self::RshPull,
        Self::DaemonPush,
        Self::DaemonPull,
    ];

    /// The cell's stable name, as `transcript-ab --cell` accepts it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RshPush => "rsh-push",
            Self::RshPull => "rsh-pull",
            Self::DaemonPush => "daemon-push",
            Self::DaemonPull => "daemon-pull",
        }
    }

    /// Looks a cell up by [`NetworkCell::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cell| cell.name() == name)
    }

    /// Whether the server sends, so the server->client stream ends with the
    /// sender's wall-clock flist timings.
    const fn is_pull(self) -> bool {
        matches!(self, Self::RshPull | Self::DaemonPull)
    }
}

impl fmt::Display for NetworkCell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// The binaries one capture runs.
#[derive(Clone, Copy, Debug)]
pub struct CellBinaries<'a> {
    /// The `oc-rsync` under test, used for the client and the server.
    pub oc_rsync: &'a Path,
    /// The `capture-rsh` trampoline for the remote-shell cells.
    pub capture_rsh: &'a Path,
}

/// Why a cell produced no transcript.
#[derive(Debug)]
pub enum CellError {
    /// The fixture, the daemon, the relay or the client could not run.
    Io(io::Error),
    /// The client ran and failed; carries its status and stderr.
    Transfer(String),
    /// The capture was missing, empty or malformed.
    Transcript(TranscriptError),
}

impl fmt::Display for CellError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cell I/O failed: {error}"),
            Self::Transfer(detail) => write!(f, "transfer failed: {detail}"),
            Self::Transcript(error) => write!(f, "transcript unusable: {error}"),
        }
    }
}

impl std::error::Error for CellError {}

impl From<io::Error> for CellError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<TranscriptError> for CellError {
    fn from(error: TranscriptError) -> Self {
        Self::Transcript(error)
    }
}

/// Deterministic, non-periodic filler (seeded xorshift).
///
/// Periodic filler creates duplicate blocks, and with duplicates the order of
/// the rolling-hash probe decides which basis block a window resolves to, so
/// two correct runs could emit different token streams.
fn filler(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}

/// Builds the cell fixture under `root`: files at three depths, every entry
/// backdated to [`FIXED_MTIME_SECS`].
///
/// `mutation` flips byte 0 of `a.bin` without changing its length or mtime,
/// so a gate can prove it sees a one-byte content change.
///
/// # Errors
///
/// Returns the first filesystem error.
pub fn build_fixture(root: &Path, mutation: bool) -> io::Result<()> {
    let sub = root.join("sub");
    let deep = sub.join("deep");
    fs::create_dir_all(&deep)?;
    let mut a = filler(0x5eed_0001, 4096);
    if mutation {
        a[0] ^= 0xff;
    }
    fs::write(root.join("a.bin"), &a)?;
    fs::write(sub.join("b.bin"), filler(0x5eed_0002, 70_000))?;
    fs::write(deep.join("c.bin"), filler(0x5eed_0003, 700))?;
    // Deepest first, so a directory's mtime is set after its children's writes.
    for path in [
        deep.join("c.bin"),
        sub.join("b.bin"),
        root.join("a.bin"),
        deep,
        sub,
        root.to_path_buf(),
    ] {
        backdate(&path)?;
    }
    Ok(())
}

fn backdate(path: &Path) -> io::Result<()> {
    let stamp = SystemTime::UNIX_EPOCH + Duration::from_secs(FIXED_MTIME_SECS);
    fs::File::open(path)?.set_modified(stamp)
}

/// Runs `cell` once from the fixture at `src` and returns its transcript.
///
/// A pull's trailing flist timings are masked
/// ([`WireTranscript::mask_pull_flist_times`]), since they are wall clock.
///
/// # Errors
///
/// Returns [`CellError`] when any part of the run fails; a failed transfer
/// carries the client's stderr.
pub fn capture_cell(
    cell: NetworkCell,
    binaries: CellBinaries<'_>,
    src: &Path,
) -> Result<WireTranscript, CellError> {
    let work = crate::create_tempdir();
    let dest = work.path().join("dest");
    fs::create_dir(&dest)?;
    let mut transcript = match cell {
        NetworkCell::RshPush | NetworkCell::RshPull => {
            capture_rsh_cell(cell, binaries, src, &dest, work.path())?
        }
        NetworkCell::DaemonPush | NetworkCell::DaemonPull => {
            capture_daemon_cell(cell, binaries.oc_rsync, src, &dest, work.path())?
        }
    };
    if cell.is_pull() {
        transcript.mask_pull_flist_times()?;
    }
    Ok(transcript)
}

fn capture_rsh_cell(
    cell: NetworkCell,
    binaries: CellBinaries<'_>,
    src: &Path,
    dest: &Path,
    work: &Path,
) -> Result<WireTranscript, CellError> {
    let recorder = TranscriptRecorder::new(work);
    let mut cmd = recorder.command(binaries.oc_rsync, binaries.oc_rsync, binaries.capture_rsh);
    cmd.arg("-rt");
    if cell == NetworkCell::RshPush {
        cmd.arg(format!("{}/", src.display()))
            .arg(format!("cellhost:{}", dest.display()));
    } else {
        cmd.arg(format!("cellhost:{}/", src.display())).arg(dest);
    }
    run_client(&mut cmd)?;
    Ok(recorder.finish()?)
}

fn capture_daemon_cell(
    cell: NetworkCell,
    oc_rsync: &Path,
    src: &Path,
    dest: &Path,
    work: &Path,
) -> Result<WireTranscript, CellError> {
    let module_root = if cell == NetworkCell::DaemonPush {
        dest
    } else {
        src
    };
    let config = write_daemon_config(work, module_root)?;
    let daemon = DaemonProcess::spawn_binary(oc_rsync, &config, ["--log-file", "/dev/null"])?;
    let relay = TeeRelay::start(SocketAddr::from((Ipv4Addr::LOCALHOST, daemon.port())))?;
    let url = format!("rsync://127.0.0.1:{}/{MODULE}/", relay.port);

    let mut cmd = normalized_client(oc_rsync);
    cmd.arg("-rt");
    if cell == NetworkCell::DaemonPush {
        cmd.arg(format!("{}/", src.display())).arg(&url);
    } else {
        cmd.arg(&url).arg(dest);
    }
    let outcome = run_client(&mut cmd);
    let transcript = relay.finish();
    outcome?;
    transcript.map_err(|error| {
        CellError::Io(io::Error::new(
            error.kind(),
            format!(
                "{error}; daemon stderr: {:?}",
                String::from_utf8_lossy(&daemon.stderr())
            ),
        ))
    })
}

/// A module rooted at `module_root`, writable and served without chroot.
///
/// A daemon started as root switches to `nobody` by default
/// (upstream: loadparm.c `uid`/`gid` defaults), which could not write the
/// fixture, so a root run pins the fixture owner instead. A non-root daemon
/// keeps its own ids and must not be given any: it cannot `setgid`.
fn write_daemon_config(work: &Path, module_root: &Path) -> io::Result<std::path::PathBuf> {
    let meta = fs::metadata(module_root)?;
    let ids = if meta.uid() == 0 {
        format!("uid = {}\ngid = {}\n", meta.uid(), meta.gid())
    } else {
        String::new()
    };
    let config = work.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\n{ids}\n[{MODULE}]\n    path = {}\n    read only = false\n",
            module_root.display()
        ),
    )?;
    Ok(config)
}

fn run_client(cmd: &mut std::process::Command) -> Result<(), CellError> {
    let output = cmd.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(CellError::Transfer(format!(
            "{}; stderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// A one-connection loopback relay that forwards client<->daemon bytes and
/// records each direction.
///
/// Each direction is pumped by its own thread and forwards a chunk as soon as
/// one `read` returns: the daemon handshake is a strict request/reply exchange,
/// so a relay that waited to fill a buffer would deadlock both peers. A
/// direction's EOF is passed on as a write shutdown, so the peer sees the same
/// half-close a direct connection would give it.
struct TeeRelay {
    port: u16,
    worker: JoinHandle<io::Result<WireTranscript>>,
}

impl TeeRelay {
    fn start(daemon: SocketAddr) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let worker = thread::spawn(move || {
            let (client, _) = listener.accept()?;
            let server = TcpStream::connect_timeout(&daemon, CLIENT_IO_TIMEOUT)?;
            for stream in [&client, &server] {
                stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
            }
            let (client_in, server_out) = (client.try_clone()?, server.try_clone()?);
            let upstream = thread::spawn(move || pump(client_in, server_out));
            let server_to_client = pump(server, client)?;
            let client_to_server = upstream
                .join()
                .map_err(|_| io::Error::other("client->daemon pump panicked"))??;
            Ok(WireTranscript {
                client_to_server,
                server_to_client,
            })
        });
        Ok(Self { port, worker })
    }

    /// Waits for both directions to close and returns what crossed them.
    fn finish(self) -> io::Result<WireTranscript> {
        self.worker
            .join()
            .map_err(|_| io::Error::other("relay thread panicked"))?
    }
}

/// Copies `from` into `to` until `from` reaches EOF, returning every byte read.
///
/// A peer that stops accepting bytes ends forwarding but not recording: the
/// capture is what this side sent, whether or not the other side read it. A
/// read timeout is an error, so a wedged session fails the cell.
fn pump(mut from: TcpStream, mut to: TcpStream) -> io::Result<Vec<u8>> {
    let mut seen = Vec::new();
    let mut buf = [0u8; 65536];
    let mut forwarding = true;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        seen.extend_from_slice(&buf[..n]);
        if forwarding && to.write_all(&buf[..n]).is_err() {
            forwarding = false;
        }
    }
    let _ = to.shutdown(Shutdown::Write);
    Ok(seen)
}
