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
//! One input cannot be pinned from outside: on a PULL the server sender ends
//! its stream with `flist_buildtime` and `flist_xfertime`, wall-clock
//! milliseconds (upstream: main.c:357-358 handle_stats(), timed in
//! flist.c:3016-3044 send_file_list()). A pull comparison masks them with
//! [`WireTranscript::mask_pull_flist_times`].
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

/// Tag of an `MSG_DATA` multiplex frame (upstream: rsync.h:210 `MPLEX_BASE`).
const MPLEX_BASE: u8 = 7;

/// Encoded width of a `varlong30(x, 3)` field for `x < 2^23`.
///
/// upstream: io.c write_varlong() - with `min_bytes == 3` a value below
/// 2^23 (about 2.3 hours of milliseconds) is always exactly 3 bytes, head
/// byte below 0x80.
const VARLONG30_WIDTH: usize = 3;

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

    /// Zero the server sender's `flist_buildtime` and `flist_xfertime` stats
    /// fields in a PULL's server->client stream.
    ///
    /// Both are wall-clock milliseconds (upstream: main.c:357-358
    /// handle_stats()), so a loaded host moves them between otherwise
    /// identical runs without changing the stream length. The stream is
    /// demultiplexed from the end of the unmultiplexed handshake (the last
    /// handshake write is the fixed checksum seed; upstream: compat.c
    /// setup_protocol()); the final `MSG_DATA` frame must be the sender's
    /// lone `NDX_DONE` goodbye (upstream: main.c:933-935
    /// read_final_goodbye(), protocol >= 31), preceded by the two 3-byte
    /// time fields.
    /// Anything else is refused rather than guessed at, so a layout change
    /// fails loudly instead of masking the wrong bytes.
    pub fn mask_pull_flist_times(&mut self) -> Result<(), TranscriptError> {
        let stream = &mut self.server_to_client;
        let malformed = |reason: &'static str| TranscriptError::Malformed {
            direction: "server-to-client",
            reason,
        };
        let seed = FIXED_CHECKSUM_SEED.to_le_bytes();
        let mux_start = stream
            .windows(seed.len())
            .position(|w| w == seed)
            .ok_or_else(|| malformed("fixed checksum seed not found"))?
            + seed.len();

        let mut data: Vec<(usize, usize)> = Vec::new();
        let mut pos = mux_start;
        while pos < stream.len() {
            let header: [u8; 4] = stream
                .get(pos..pos + 4)
                .and_then(|h| h.try_into().ok())
                .ok_or_else(|| malformed("truncated multiplex header"))?;
            let header = u32::from_le_bytes(header);
            let tag = (header >> 24) as u8;
            let len = (header & 0x00ff_ffff) as usize;
            if tag < MPLEX_BASE {
                return Err(malformed("multiplex tag below MPLEX_BASE"));
            }
            pos += 4;
            if pos + len > stream.len() {
                return Err(malformed("multiplex frame runs past end of stream"));
            }
            if tag == MPLEX_BASE {
                data.push((pos, len));
            }
            pos += len;
        }

        // The sender blocks reading the client's NDX_DONE before writing its
        // own, and that read flushes the stats first (upstream: io.c
        // perform_io()), so the goodbye is always a lone 1-byte frame.
        match data.pop() {
            Some((start, 1)) if stream[start] == 0 => {}
            _ => return Err(malformed("stream does not end with an NDX_DONE frame")),
        }
        let tail_len = 2 * VARLONG30_WIDTH;
        let mut tail: Vec<usize> = data
            .iter()
            .rev()
            .flat_map(|&(start, len)| (start..start + len).rev())
            .take(tail_len)
            .collect();
        tail.reverse();
        if tail.len() != tail_len {
            return Err(malformed("data stream too short for the stats trailer"));
        }
        if stream[tail[0]] >= 0x80 || stream[tail[VARLONG30_WIDTH]] >= 0x80 {
            return Err(malformed("flist time field is not a 3-byte varlong30"));
        }
        for offset in tail {
            stream[offset] = 0;
        }
        Ok(())
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
    /// A capture does not have the wire layout a normalization step
    /// requires, so it refuses to mask bytes it cannot identify.
    Malformed {
        /// Which direction's stream is affected.
        direction: &'static str,
        /// What the layout check found.
        reason: &'static str,
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
            TranscriptError::Malformed { direction, reason } => {
                write!(f, "{direction} capture is malformed: {reason}")
            }
        }
    }
}

impl std::error::Error for TranscriptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TranscriptError::Missing { source, .. } => Some(source),
            TranscriptError::Empty { .. } | TranscriptError::Malformed { .. } => None,
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

    /// A pull's server->client shape: handshake ending in the fixed seed,
    /// then multiplexed frames. `frames` are `(tag, payload)` pairs.
    fn pull_stream(frames: &[(u8, &[u8])]) -> Vec<u8> {
        let mut out = 32u32.to_le_bytes().to_vec();
        out.extend_from_slice(&FIXED_CHECKSUM_SEED.to_le_bytes());
        for &(tag, payload) in frames {
            let header = (u32::from(tag) << 24) | payload.len() as u32;
            out.extend_from_slice(&header.to_le_bytes());
            out.extend_from_slice(payload);
        }
        out
    }

    /// Stats payload: total_read, total_written, total_size, then the two
    /// time fields, each a 3-byte varlong30 (`[byte2, byte0, byte1]`).
    fn stats(build_ms: u8, xfer_ms: u8) -> Vec<u8> {
        vec![
            0, 0x8d, 0, 0, 0x41, 0x23, 0, 0x65, 0x21, 0, build_ms, 0, 0, xfer_ms, 0,
        ]
    }

    fn masked(stream: Vec<u8>) -> Result<Vec<u8>, TranscriptError> {
        let mut t = WireTranscript {
            client_to_server: b"c".to_vec(),
            server_to_client: stream,
        };
        t.mask_pull_flist_times()?;
        Ok(t.server_to_client)
    }

    #[test]
    fn mask_equates_runs_that_differ_only_in_flist_times() {
        // Why: the sender reports wall-clock flist timings, so a loaded CI
        // host produced same-length, different-byte pulls. Masking must
        // remove exactly that variance and nothing else.
        let slow = pull_stream(&[(7, b"flist"), (7, &stats(1, 2)), (7, &[0])]);
        let fast = pull_stream(&[(7, b"flist"), (7, &stats(0, 0)), (7, &[0])]);
        assert_ne!(slow, fast);
        assert_eq!(masked(slow).unwrap(), masked(fast.clone()).unwrap());
        assert_eq!(
            masked(fast.clone()).unwrap(),
            fast,
            "zero times are a fixpoint"
        );
    }

    #[test]
    fn mask_keeps_the_deterministic_stats_fields() {
        // Why: total_size and friends are real transfer evidence; a mask
        // that also hid them would blind the gate to content changes.
        let base = pull_stream(&[(7, &stats(3, 4)), (7, &[0])]);
        let mut other_size = stats(3, 4);
        other_size[8] ^= 1;
        let moved = pull_stream(&[(7, &other_size), (7, &[0])]);
        assert_ne!(masked(base).unwrap(), masked(moved).unwrap());
    }

    #[test]
    fn mask_follows_data_across_frame_boundaries_and_skips_other_tags() {
        // Why: buffer flushes can split the trailer across MSG_DATA frames,
        // and non-data frames (MSG_INFO here) carry no stats bytes.
        let whole = stats(5, 6);
        let split = pull_stream(&[
            (7, &whole[..11]),
            (7 + 2, b"info"),
            (7, &whole[11..]),
            (7, &[0]),
        ]);
        let masked_split = masked(split).unwrap();
        let expect = pull_stream(&[
            (7, &stats(0, 0)[..11]),
            (7 + 2, b"info"),
            (7, &stats(0, 0)[11..]),
            (7, &[0]),
        ]);
        assert_eq!(masked_split, expect);
    }

    #[test]
    fn mask_refuses_a_stream_without_the_goodbye_trailer() {
        // Why: masking by position is only sound when the layout is the
        // one read_final_goodbye() produces; otherwise refuse loudly.
        let no_goodbye = pull_stream(&[(7, &stats(1, 2))]);
        assert!(matches!(
            masked(no_goodbye),
            Err(TranscriptError::Malformed { reason, .. }) if reason.contains("NDX_DONE")
        ));
        let overrun = {
            let mut s = pull_stream(&[(7, &stats(1, 2)), (7, &[0])]);
            s.pop();
            s
        };
        assert!(matches!(
            masked(overrun),
            Err(TranscriptError::Malformed { .. })
        ));
        let unseeded = vec![0u8; 32];
        assert!(matches!(
            masked(unseeded),
            Err(TranscriptError::Malformed { reason, .. }) if reason.contains("seed")
        ));
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
