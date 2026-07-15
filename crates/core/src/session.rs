//! Session-level pipeline driver selection and tokio runtime ownership shim.
//!
//! `core` owns the choice of transfer pipeline driver. The public entry point
//! (`run_server_stdio`) keeps the exact same signature and
//! `Result<ServerStats, io::Error>` shape regardless of the `tokio-transfer`
//! feature - the feature only swaps the *driver*, never the API
//! (`docs/design/asy-2-tokio-runtime-feature.md` section 4).
//!
//! # Runtime ownership (ASY-3)
//!
//! Per section 5 of the design, `core` is the only crate permitted to build a
//! tokio runtime for the transfer path. When `tokio-transfer` is on,
//! `run_server_stdio` probes `tokio::runtime::Handle::try_current()`:
//!
//! - If an ambient runtime exists (e.g. the SSH transport's current-thread
//!   runtime), its handle is **adopted** and the transfer future is driven on
//!   it without building a second runtime.
//! - Otherwise a session-scoped `Builder::new_current_thread()` runtime is
//!   built and the future is driven with `block_on`.
//!
//! Crates below `core` (`transfer`, `engine`, `fast_io`) only ever receive a
//! `tokio::runtime::Handle`; they never build a runtime. No tokio type
//! appears in any public `core` signature.
//!
//! When `tokio-transfer` is off, `run_server_stdio` forwards directly to the
//! threaded `transfer::run_server_stdio` with zero tokio in the path.

use std::io::{Read, Write};

use crate::server::{ServerConfig, ServerResult};
use transfer::TransferProgressCallback;

/// Executes the native server over standard I/O, selecting the pipeline driver
/// according to the `tokio-transfer` feature.
///
/// This is the session-level facade the `--server` entry point calls. Its
/// signature and result are identical whether or not `tokio-transfer` is
/// enabled; only the driver behind it changes.
///
/// # Errors
///
/// Propagates every error from the underlying server body unchanged.
#[cfg(not(feature = "tokio-transfer"))]
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    progress: Option<&mut dyn TransferProgressCallback>,
) -> ServerResult {
    // Threaded production path: byte-for-byte the pre-ASY-3 behaviour, no tokio.
    transfer::run_server_stdio(config, stdin, stdout, progress)
}

/// Tokio-hosted variant. See the module docs for the runtime-ownership rules.
///
/// # Errors
///
/// Propagates every error from the underlying server body unchanged.
#[cfg(feature = "tokio-transfer")]
pub fn run_server_stdio(
    config: ServerConfig,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
    progress: Option<&mut dyn TransferProgressCallback>,
) -> ServerResult {
    // The handshake is intrinsically part of the server body; perform it inside
    // the same driver invocation so the tokio path is a drop-in for the
    // threaded `transfer::run_server_stdio` (which does handshake + transfer).
    // upstream: compat.c:600-602 - reconcile a pre-release peer's subprotocol
    // (carried in its `-e` capability string) before advertising our version.
    // Wire-identical to a plain handshake for any stock release peer.
    let handshake = transfer::perform_server_handshake(stdin, stdout, &config.flag_string)?;
    with_transfer_runtime(|handle| {
        transfer::run_server_with_handshake_on(
            handle, config, handshake, stdin, stdout, progress, None, None,
        )
    })
}

/// Runs `f` with a tokio runtime handle, adopting an ambient runtime when one
/// exists and otherwise building a session-scoped current-thread runtime.
///
/// This is the single place in the workspace that constructs a runtime for the
/// transfer pipeline. `f` receives a borrowed [`tokio::runtime::Handle`] and
/// must not outlive it.
#[cfg(feature = "tokio-transfer")]
fn with_transfer_runtime<R>(f: impl FnOnce(&tokio::runtime::Handle) -> R) -> R {
    match tokio::runtime::Handle::try_current() {
        // Adopt an already-running runtime (e.g. the SSH transport's
        // current-thread runtime). We must not build a nested runtime here.
        Ok(handle) => f(&handle),
        // No ambient runtime: build one scoped to this session. current_thread
        // keeps the future on the calling thread so borrowed transports stay
        // valid and wire ordering matches the threaded path. The multi-thread
        // flavour is reserved for the daemon (design section 5).
        Err(_) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build session-scoped tokio runtime");
            f(runtime.handle())
        }
    }
}

#[cfg(all(test, feature = "tokio-transfer"))]
mod tests {
    use super::with_transfer_runtime;

    /// ASY-3 open-question 1: with no ambient runtime, the shim builds a
    /// session-scoped current-thread runtime and runs the closure on it. The
    /// closure can drive its own `block_on` on the supplied handle (the shape
    /// the transfer driver uses).
    #[test]
    fn shim_builds_runtime_when_none_ambient() {
        let out = with_transfer_runtime(|handle| {
            // Driving a future on the built handle must succeed and run on this
            // thread (current-thread runtime), mirroring the driver's use.
            handle.block_on(async { 7u32 })
        });
        assert_eq!(out, 7);
    }

    /// With an ambient multi-thread runtime, the shim adopts its handle rather
    /// than building a second runtime. `block_in_place` is used to prove the
    /// adopted handle is the ambient one without a nested `block_on` (which
    /// would panic on the current-thread flavour).
    #[test]
    fn shim_adopts_ambient_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let ambient_id = rt.handle().id();
        let adopted_id = rt.block_on(async {
            // Inside the runtime context, try_current() must resolve, so the
            // shim adopts it. Capture the adopted handle's id to compare.
            tokio::task::block_in_place(|| with_transfer_runtime(|handle| handle.id()))
        });
        assert_eq!(
            adopted_id, ambient_id,
            "shim must adopt the ambient runtime, not build a new one"
        );
    }
}
