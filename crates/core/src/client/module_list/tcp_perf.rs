//! Client-side TCP performance socket options applied to daemon
//! connections.
//!
//! Wraps `TCP_NOTSENT_LOWAT` under a best-effort apply path so unsupported
//! platforms become silent no-ops. Client-side `TCP_FASTOPEN` is deferred
//! to a follow-up that wires a `sendto(MSG_FASTOPEN)` adapter; the
//! [`TcpFastOpenMode`] argument is kept on the signature so the call site
//! does not need to change when that work lands.
//!
//! Wire-compatible with upstream rsync: both options only touch kernel
//! socket behaviour and never alter the rsync protocol stream.

use std::net::TcpStream;

use fast_io::{
    DEFAULT_TCP_NOTSENT_LOWAT, set_tcp_notsent_lowat, set_tcp_quickack,
    tcp_notsent_lowat_supported, tcp_quickack_supported,
};

use crate::client::TcpFastOpenMode;

/// Apply client-side perf options to a connected stream.
///
/// Sets `TCP_NOTSENT_LOWAT` when the platform supports it. The Linux
/// client-side TFO path requires `MSG_FASTOPEN` on the first `sendto(2)`,
/// which is incompatible with the standard `connect`/`write` flow used by
/// the rsync client; client-side TFO is deferred to a follow-up that
/// wires a `sendto` adapter.
pub(crate) fn apply_client_tcp_perf_options(stream: &TcpStream, mode: TcpFastOpenMode) {
    let _ = mode; // Reserved for future client-side TFO wiring.
    if tcp_notsent_lowat_supported() {
        // Errors are best-effort: `TCP_NOTSENT_LOWAT` is an optimisation
        // hint and a failing setsockopt is non-fatal.
        let _ = set_tcp_notsent_lowat(stream, DEFAULT_TCP_NOTSENT_LOWAT);
    }
    if tcp_quickack_supported() {
        // One-shot hint to skip the delayed-ACK timer on the first ACK;
        // best-effort, non-fatal.
        let _ = set_tcp_quickack(stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn client_perf_options_apply_silently_for_all_modes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        let addr = listener.local_addr().expect("addr");
        let join = std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let stream = TcpStream::connect(addr).expect("connect");

        // No mode should panic or error out at this layer; unsupported
        // platforms turn each call into a no-op.
        for mode in [
            TcpFastOpenMode::Auto,
            TcpFastOpenMode::On,
            TcpFastOpenMode::Off,
        ] {
            apply_client_tcp_perf_options(&stream, mode);
        }

        drop(stream);
        join.join().expect("accept thread completes");
    }
}
