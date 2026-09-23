//! Wire-transcript capture for byte-neutrality gates.
//!
//! [`TranscriptRecorder`] wires an oc-rsync client invocation through the
//! `capture-rsh` teeing trampoline (`src/bin/capture-rsh.rs`) so the full
//! byte stream of BOTH directions of a client<->server transfer lands in two
//! files, then reads them back as a [`WireTranscript`] for byte-identity
//! comparison between two binaries or configurations. First consumer: the
//! lazy-flist programme's gate that a staged producer change with its flag
//! off is byte-identical to the eager producer (LF-0b/LF-8a).
//!
//! # Normalization
//!
//! A raw transcript is comparable across runs only once the deliberately
//! variable protocol inputs are pinned. [`TranscriptRecorder::command`] owns
//! that set, so every consumer states the same baseline:
//!
//! - `--checksum-seed=<fixed>`: without it the server seeds from
//!   `time(NULL) ^ (getpid() << 6)` (upstream: compat.c:824
//!   setup_protocol()), which perturbs the seed word on the wire and every
//!   downstream checksum.
//! - `RSYNC_CHECKSUM_LIST` pinned to one value: the negotiation
//!   advertisement is read from this variable (upstream: compat.c:410-421
//!   getenv_nstr(), including the `&`-split of client/server halves), so an
//!   inherited value would move the advertised name list between arms.
//! - `RSYNC_COMPRESS_LIST` removed: same reader, compression side.
//! - `RSYNC_PROTECT_ARGS` removed: secluded args relocate the operand argv
//!   onto the wire itself, coupling the transcript to per-run tempdir names.
//! - `OC_CONSECUTIVE_MATCH` removed: an opt-in oc extension that changes the
//!   advertised `-e` capability letters and the signature layout.
//! - `OC_RSYNC_LAZY_FLIST` removed: the staging flag this harness gates; a
//!   cell that wants an arm set re-sets it after `command()`.
//!
//! Fixture-side variance (file mtimes in the flist, quick-check outcomes)
//! is the caller's to pin: backdate every fixture entry to a fixed mtime.
//!
//! The trampoline is resolved through [`crate::workspace_bin`], so a missing
//! or stale `capture-rsh` build is a loud panic naming the path, never a
//! silent skip.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable naming the client-to-server capture file.
pub const TRANSCRIPT_C2S_ENV: &str = "OC_TRANSCRIPT_C2S";

/// Environment variable naming the server-to-client capture file.
pub const TRANSCRIPT_S2C_ENV: &str = "OC_TRANSCRIPT_S2C";

/// Fixed `--checksum-seed` value applied by [`TranscriptRecorder::command`].
pub const FIXED_CHECKSUM_SEED: u32 = 32761;

/// Fixed `RSYNC_CHECKSUM_LIST` applied to both arms of a comparison.
///
/// Any single value works; what matters is that BOTH arms advertise the same
/// list (upstream: compat.c:410-421 getenv_nstr()).
pub const FIXED_CHECKSUM_LIST: &str = "md5";

/// The raw bytes of one transfer, split by direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireTranscript {
    /// Every byte the client wrote toward the server.
    pub client_to_server: Vec<u8>,
    /// Every byte the server wrote toward the client.
    pub server_to_client: Vec<u8>,
}

impl WireTranscript {
    /// Total captured bytes across both directions.
    #[must_use]
    pub fn total_len(&self) -> usize {
        self.client_to_server.len() + self.server_to_client.len()
    }
}

/// Error reading back a capture: the instrument itself failed.
#[derive(Debug)]
pub enum TranscriptError {
    /// A capture file could not be read at all - the trampoline never ran or
    /// never created its sink.
    Missing {
        /// Which direction's file is affected.
        direction: &'static str,
        /// The capture file path.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// A capture file exists but holds zero bytes. A real rsync session
    /// moves bytes in BOTH directions, so an empty capture means the
    /// instrument saw nothing - comparing two empty captures would be a
    /// vacuous pass.
    Empty {
        /// Which direction's file is affected.
        direction: &'static str,
        /// The capture file path.
        path: PathBuf,
    },
}

impl std::fmt::Display for TranscriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TranscriptError::Missing {
                direction,
                path,
                source,
            } => write!(
                f,
                "{direction} capture unreadable at {}: {source}",
                path.display()
            ),
            TranscriptError::Empty { direction, path } => write!(
                f,
                "{direction} capture at {} is EMPTY - the trampoline forwarded no bytes",
                path.display()
            ),
        }
    }
}

impl std::error::Error for TranscriptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TranscriptError::Missing { source, .. } => Some(source),
            TranscriptError::Empty { .. } => None,
        }
    }
}

/// One capture slot: a pair of capture-file paths plus the command wiring
/// that routes an oc-rsync client's remote shell through `capture-rsh`.
pub struct TranscriptRecorder {
    c2s: PathBuf,
    s2c: PathBuf,
}

impl TranscriptRecorder {
    /// Create a recorder whose capture files live under `dir`.
    ///
    /// `dir` must exist; a fresh recorder (or a fresh directory) per run
    /// keeps two runs from appending into one file.
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self {
            c2s: dir.join("client-to-server.bin"),
            s2c: dir.join("server-to-client.bin"),
        }
    }

    /// Build the client [`Command`], pre-wired for capture and normalized
    /// per the module contract above.
    ///
    /// `client` is spawned; `server` is passed as `--rsync-path`, so the
    /// trampoline execs it as the `--server` peer. `capture_rsh` is the
    /// teeing trampoline binary, wired in as `--rsh`; the caller supplies its
    /// path (an integration test of this crate gets it for free from
    /// `env!("CARGO_BIN_EXE_capture-rsh")`, which Cargo builds before the
    /// test runs). The caller appends its own flags and the
    /// `src`/`fakehost:dest` operands, and may override any env var AFTER
    /// this call (later `env` calls win).
    #[must_use]
    pub fn command(&self, client: &Path, server: &Path, capture_rsh: &Path) -> Command {
        let mut cmd = Command::new(client);
        cmd.arg("--rsh")
            .arg(capture_rsh)
            .arg("--rsync-path")
            .arg(server)
            .arg(format!("--checksum-seed={FIXED_CHECKSUM_SEED}"))
            .env(TRANSCRIPT_C2S_ENV, &self.c2s)
            .env(TRANSCRIPT_S2C_ENV, &self.s2c)
            .env("RSYNC_CHECKSUM_LIST", FIXED_CHECKSUM_LIST)
            .env_remove("RSYNC_COMPRESS_LIST")
            .env_remove("RSYNC_RSH")
            .env_remove("RSYNC_PROTECT_ARGS")
            .env_remove("OC_CONSECUTIVE_MATCH")
            .env_remove("OC_RSYNC_LAZY_FLIST");
        cmd
    }

    /// Read both captures back, refusing missing or empty evidence.
    pub fn finish(&self) -> Result<WireTranscript, TranscriptError> {
        let read = |direction: &'static str, path: &Path| -> Result<Vec<u8>, TranscriptError> {
            let bytes = fs::read(path).map_err(|source| TranscriptError::Missing {
                direction,
                path: path.to_path_buf(),
                source,
            })?;
            if bytes.is_empty() {
                return Err(TranscriptError::Empty {
                    direction,
                    path: path.to_path_buf(),
                });
            }
            Ok(bytes)
        };
        Ok(WireTranscript {
            client_to_server: read("client-to-server", &self.c2s)?,
            server_to_client: read("server-to-client", &self.s2c)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_refuses_a_missing_capture() {
        // Why: a comparison that silently read two absent files as equal
        // would pass without the instrument ever running - Rule 12.
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = TranscriptRecorder::new(dir.path());
        match recorder.finish() {
            Err(TranscriptError::Missing { direction, .. }) => {
                assert_eq!(direction, "client-to-server");
            }
            other => panic!("expected Missing, got {other:?}"),
        }
    }

    #[test]
    fn finish_refuses_an_empty_capture() {
        // Why: an empty pair compares equal trivially; the harness must
        // treat zero forwarded bytes as instrument failure, not evidence.
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = TranscriptRecorder::new(dir.path());
        fs::write(dir.path().join("client-to-server.bin"), b"x").expect("write c2s");
        fs::write(dir.path().join("server-to-client.bin"), b"").expect("write s2c");
        match recorder.finish() {
            Err(TranscriptError::Empty { direction, .. }) => {
                assert_eq!(direction, "server-to-client");
            }
            other => panic!("expected Empty, got {other:?}"),
        }
    }

    #[test]
    fn finish_returns_both_directions_verbatim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let recorder = TranscriptRecorder::new(dir.path());
        fs::write(dir.path().join("client-to-server.bin"), b"ping").expect("write c2s");
        fs::write(dir.path().join("server-to-client.bin"), b"pong!").expect("write s2c");
        let transcript = recorder.finish().expect("both present and non-empty");
        assert_eq!(transcript.client_to_server, b"ping");
        assert_eq!(transcript.server_to_client, b"pong!");
        assert_eq!(transcript.total_len(), 9);
    }
}
