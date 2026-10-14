//! Client-side TCP performance socket options applied to daemon
//! connections.
//!
//! Applies `TCP_NOTSENT_LOWAT`, `TCP_QUICKACK` and (with `--bwlimit`)
//! `SO_MAX_PACING_RATE` on a best-effort path so unsupported platforms become
//! silent no-ops. Client-side TCP Fast Open is requested before `connect(2)`
//! in `connect_with_optional_bind`, not here.
//!
//! Deviation from upstream: rsync sets none of these options. They only touch
//! kernel socket behaviour and never alter the rsync protocol stream.

use std::net::TcpStream;

use fast_io::{
    DEFAULT_TCP_NOTSENT_LOWAT, set_so_max_pacing_rate, set_tcp_notsent_lowat, set_tcp_quickack,
    so_max_pacing_rate_supported, tcp_notsent_lowat_supported, tcp_quickack_supported,
};

use crate::client::TcpFastOpenMode;

/// Applies client-side perf options to a connected stream.
///
/// Sets `TCP_NOTSENT_LOWAT` and `TCP_QUICKACK` when the platform supports
/// them. Client-side TFO is
/// requested earlier, before `connect(2)`, via `TCP_FASTOPEN_CONNECT` in
/// `connect_with_optional_bind`; that mechanism is compatible with the
/// standard `connect`/`write` flow, so this post-connect helper does not act
/// on `mode`.
///
/// When `pacing_bytes_per_sec` is `Some` (the client `--bwlimit` rate),
/// `SO_MAX_PACING_RATE` caps the kernel send pace to match. This is a
/// complementary hint: the userspace token-bucket limiter remains
/// authoritative for correctness while the kernel smooths bursts.
pub(crate) fn apply_client_tcp_perf_options(
    stream: &TcpStream,
    mode: TcpFastOpenMode,
    pacing_bytes_per_sec: Option<u32>,
) {
    let _ = mode; // TFO is applied pre-connect; this helper is post-connect.
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
    if let Some(rate) = pacing_bytes_per_sec
        && so_max_pacing_rate_supported()
    {
        // Kernel pacing hint mirroring `--bwlimit`; best-effort, non-fatal.
        let _ = set_so_max_pacing_rate(stream, rate);
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
            // Exercise both the no-pacing and pacing-hint paths.
            apply_client_tcp_perf_options(&stream, mode, None);
            apply_client_tcp_perf_options(&stream, mode, Some(1_000_000));
        }

        drop(stream);
        join.join().expect("accept thread completes");
    }
}
