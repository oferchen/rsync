//! Unit tests for [`SpillableReorderBuffer`].
//!
//! Lives at the buffer-module level so the test bodies can reach the private
//! fields (`spill_file`, `spill_index`, `memory_used`, `inner`) and constants
//! ([`SPILL_TAG_RAW`], [`SPILL_TAG_ZSTD`], [`HOT_ZONE`]) without exposing them
//! to crate-external callers. The suite is split by concern so each file
//! stays under the workspace LoC cap.

use std::io::{self, Read, Write};

use super::super::{SpillCodec, SpillableReorderBuffer};

mod adaptive;
mod basic;
mod compression;
mod enospc_degradation;
mod fault;
mod granularity;
mod hardening;
mod in_memory_only;
mod memory_pressure;
mod preflight;
mod reclaim;

/// Simple [`SpillCodec`] for `u64` shared across every suite.
impl SpillCodec for u64 {
    fn encode(&self, w: &mut dyn Write) -> io::Result<()> {
        w.write_all(&self.to_le_bytes())
    }

    fn decode(r: &mut dyn Read) -> io::Result<Self> {
        let mut buf = [0u8; 8];
        r.read_exact(&mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn estimated_size(&self) -> usize {
        8
    }
}

/// Codec wrapper whose `encode` fails on demand. Used to inject ENOSPC
/// and partial-write scenarios without touching the real filesystem.
#[derive(Clone, Copy)]
pub(super) struct FailingCodec {
    pub(super) value: u64,
    pub(super) size: usize,
    pub(super) fail_kind: Option<io::ErrorKind>,
}

impl SpillCodec for FailingCodec {
    fn encode(&self, w: &mut dyn Write) -> io::Result<()> {
        if let Some(kind) = self.fail_kind {
            return Err(io::Error::new(kind, "injected encode failure"));
        }
        w.write_all(&self.value.to_le_bytes())?;
        // Pad to claimed size so memory accounting matches.
        if self.size > 8 {
            w.write_all(&vec![0u8; self.size - 8])?;
        }
        Ok(())
    }

    fn decode(r: &mut dyn Read) -> io::Result<Self> {
        let mut buf = [0u8; 8];
        r.read_exact(&mut buf)?;
        Ok(Self {
            value: u64::from_le_bytes(buf),
            size: 8,
            fail_kind: None,
        })
    }

    fn estimated_size(&self) -> usize {
        self.size
    }
}

/// Drains the buffer in full and panics on any spill I/O failure.
pub(super) fn drain_all<T: SpillCodec>(buf: &mut SpillableReorderBuffer<T>) -> Vec<T> {
    buf.drain_ready().expect("drain should succeed")
}
