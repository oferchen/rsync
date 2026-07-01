//! Tokio-hosted driver for the server transfer pipeline (ASY-3 foundation).
//!
//! This module is the `tokio-transfer` counterpart to the threaded call into
//! [`crate::run_server_with_handshake`]. For the ASY-3 rung it hosts the
//! **existing synchronous** server body on a tokio runtime so the runtime
//! ownership and wire-in scaffold can be proven end-to-end without converting
//! any per-boundary blocking site to `.await`. Those conversions land in
//! ASY-7/8 (see `docs/design/asy-3-async-boundary-spec.md`).
//!
//! # Byte-for-byte parity
//!
//! The driver runs the **same** [`crate::run_server_with_handshake`] code path
//! as the threaded caller. The only difference is that the call is issued from
//! inside a tokio `block_on` future rather than directly on the caller stack.
//! The current-thread runtime executes the future on the calling thread, so the
//! borrowed `stdin` / `stdout` transports stay valid and every wire byte, flush
//! ordering, checksum, and goodbye handshake is identical to the threaded path.
//! The hosting-primitive equivalence is asserted by the unit tests in this
//! module; the full-transfer wire-byte parity is proven by a real oc-rsync
//! self-transfer (client vs `--rsh` server) with a pinned `--checksum-seed`.
//!
//! # Runtime ownership
//!
//! Per `docs/design/asy-2-tokio-runtime-feature.md` section 5, no crate below
//! `core` builds a runtime. The driver receives a [`tokio::runtime::Handle`]
//! from `core`'s session shim and uses [`Handle::block_on`] to drive the
//! future. `core` decides whether that handle was adopted from an ambient
//! runtime or built as a session-scoped current-thread runtime.

use std::io::{Read, Write};

use tokio::runtime::Handle;

use crate::{
    BatchRecording, HandshakeResult, ItemizeCallback, ServerConfig, ServerResult,
    TransferProgressCallback, run_server_with_handshake,
};

/// Drives [`run_server_with_handshake`] on the supplied tokio runtime handle.
///
/// This is the ASY-3 tokio entry point. It is signature-compatible with
/// [`run_server_with_handshake`] except for the leading `handle` argument,
/// which is the runtime the future is driven on. The synchronous server body
/// runs inside the `block_on` future on the current thread, so borrowed
/// transports remain valid and the wire output is byte-identical to the
/// threaded path.
///
/// No tokio type appears in the return value: the caller in `core` sees the
/// same [`ServerResult`] the threaded path returns. `handle` is an internal
/// implementation detail passed down from the `core` session shim and never
/// escapes into a public `core` signature.
///
/// # Errors
///
/// Propagates every error from [`run_server_with_handshake`] unchanged.
pub fn run_server_with_handshake_on<W: Write>(
    handle: &Handle,
    config: ServerConfig,
    handshake: HandshakeResult,
    stdin: &mut dyn Read,
    stdout: W,
    progress: Option<&mut dyn TransferProgressCallback>,
    batch: Option<BatchRecording>,
    itemize: Option<&mut dyn ItemizeCallback>,
) -> ServerResult {
    // Run the synchronous server body inside the runtime. ASY-7/8 replace the
    // inline sync call with per-boundary `.await` sites; ASY-3 only establishes
    // the runtime-hosted scaffold.
    host_sync_on(handle, move || {
        run_server_with_handshake(config, handshake, stdin, stdout, progress, batch, itemize)
    })
}

/// Runs a synchronous closure inside the runtime `handle` via `block_on`.
///
/// `block_on` on a current-thread runtime executes the future on the calling
/// thread, so the closure may borrow non-`Send` / non-`'static` state (the
/// borrowed `stdin` / `stdout` transports) and still run to completion. This is
/// the single hosting primitive the ASY-3 driver uses; ASY-7/8 replace the body
/// of the hosted closure with `.await` sites without changing this shape.
fn host_sync_on<R>(handle: &Handle, f: impl FnOnce() -> R) -> R {
    handle.block_on(async move { f() })
}

#[cfg(test)]
mod tests {
    use super::host_sync_on;
    use std::io::Write;
    use tokio::runtime::Builder;

    /// The runtime-hosting primitive preserves borrowed-state access and returns
    /// the closure's value unchanged. This is the ASY-3 invariant that lets the
    /// driver host the sync server body without moving borrowed transports into
    /// `spawn_blocking`.
    #[test]
    fn host_sync_on_runs_closure_with_borrows_and_returns_value() {
        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let mut out: Vec<u8> = Vec::new();
        let ret = host_sync_on(rt.handle(), || {
            out.write_all(b"wire-bytes").unwrap();
            42u32
        });
        assert_eq!(ret, 42);
        assert_eq!(out, b"wire-bytes");
    }

    /// Hosting a closure on the runtime yields byte-identical output to running
    /// it directly. This is the mechanical core of the tokio-driver parity
    /// claim: the runtime only changes *where* the sync body runs, not *what* it
    /// writes.
    #[test]
    fn hosted_output_matches_direct_output() {
        let payload: &[u8] = b"the quick brown fox\x00\x01\x02frame";

        let mut direct = Vec::new();
        direct.write_all(payload).unwrap();

        let rt = Builder::new_current_thread().enable_all().build().unwrap();
        let mut hosted = Vec::new();
        host_sync_on(rt.handle(), || hosted.write_all(payload).unwrap());

        assert_eq!(direct, hosted, "hosted wire output must be byte-identical");
    }
}
