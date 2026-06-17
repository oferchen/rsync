//! Integration tests for the TCP_FASTOPEN and TCP_NOTSENT_LOWAT helpers
//! exposed by `fast_io::socket_options`.
//!
//! Tests exercise the public API surface only; per-platform behaviour is
//! reported through the boolean return values and the helper constants so
//! the suite stays portable across Linux, macOS, Windows, and other
//! targets.

use std::net::{TcpListener, TcpStream};

use fast_io::{
    DEFAULT_TCP_FASTOPEN_QLEN, DEFAULT_TCP_NOTSENT_LOWAT, enable_tcp_fastopen_listener,
    set_listener_int_option, set_socket_int_option, set_tcp_notsent_lowat,
    tcp_fastopen_listener_supported, tcp_notsent_lowat_supported,
};

fn connected_pair() -> (TcpStream, TcpStream, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind listener");
    let addr = listener.local_addr().expect("local addr");
    let handle = std::thread::spawn(move || {
        // Hold the accept handle alive for the test lifetime so the kernel
        // does not unceremoniously close the peer.
        let (mut peer, _) = listener.accept().expect("accept");
        let _ = peer.set_read_timeout(Some(std::time::Duration::from_millis(50)));
        let mut buf = [0u8; 1];
        let _ = std::io::Read::read(&mut peer, &mut buf);
    });
    let stream = TcpStream::connect(addr).expect("client connect");
    let dummy_peer = stream.try_clone().expect("clone client side");
    (stream, dummy_peer, handle)
}

#[test]
fn defaults_have_expected_values() {
    assert_eq!(DEFAULT_TCP_FASTOPEN_QLEN, 128);
    assert_eq!(DEFAULT_TCP_NOTSENT_LOWAT, 64 * 1024);
}

#[test]
fn listener_tfo_outcome_matches_platform_support_flag() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let outcome = enable_tcp_fastopen_listener(&listener, DEFAULT_TCP_FASTOPEN_QLEN);

    match outcome {
        Ok(true) => {
            assert!(
                tcp_fastopen_listener_supported(),
                "Ok(true) must agree with tcp_fastopen_listener_supported()"
            );
        }
        Ok(false) => {
            assert!(
                !tcp_fastopen_listener_supported(),
                "Ok(false) must agree with tcp_fastopen_listener_supported()"
            );
        }
        Err(error) => {
            // Sysctl/permissions can cause Linux to refuse TFO even when
            // the platform supports the option. Accept the error path but
            // assert the platform claims support; otherwise it would be a
            // genuine portability bug.
            assert!(
                tcp_fastopen_listener_supported(),
                "TFO error on a platform that reports unsupported: {error}"
            );
        }
    }
}

#[test]
fn notsent_lowat_outcome_matches_platform_support_flag() {
    let (stream, _peer, handle) = connected_pair();
    let outcome = set_tcp_notsent_lowat(&stream, DEFAULT_TCP_NOTSENT_LOWAT);

    match outcome {
        Ok(true) => assert!(tcp_notsent_lowat_supported()),
        Ok(false) => assert!(!tcp_notsent_lowat_supported()),
        Err(error) => panic!("TCP_NOTSENT_LOWAT setsockopt failed unexpectedly: {error}"),
    }

    drop(stream);
    handle.join().expect("accept thread completes");
}

#[test]
fn notsent_lowat_accepts_small_and_large_watermarks() {
    if !tcp_notsent_lowat_supported() {
        return;
    }

    let (stream, _peer, handle) = connected_pair();

    set_tcp_notsent_lowat(&stream, 4 * 1024).expect("4k watermark");
    set_tcp_notsent_lowat(&stream, 1024 * 1024).expect("1m watermark");

    drop(stream);
    handle.join().expect("accept thread completes");
}

#[test]
fn listener_setsockopt_round_trip_is_independent_of_stream_helper() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");

    // SO_REUSEADDR is always-on and writable on every supported target;
    // it is a stable sanity check for the listener-side helper without
    // requiring TFO support.
    #[cfg(unix)]
    let (level, option) = (libc::SOL_SOCKET, libc::SO_REUSEADDR);
    #[cfg(windows)]
    let (level, option) = (0xFFFF_i32, 0x0004_i32);

    set_listener_int_option(&listener, level, option, 1).expect("listener setsockopt");
}

#[test]
fn stream_setsockopt_round_trip_still_works() {
    let (stream, _peer, handle) = connected_pair();

    #[cfg(unix)]
    let (level, option) = (libc::SOL_SOCKET, libc::SO_KEEPALIVE);
    #[cfg(windows)]
    let (level, option) = (0xFFFF_i32, 0x0008_i32);

    set_socket_int_option(&stream, level, option, 1).expect("stream setsockopt");

    drop(stream);
    handle.join().expect("accept thread completes");
}

#[test]
fn platform_support_flags_are_self_consistent() {
    // Compile-time facts about supported platforms must match the runtime
    // flags exposed to callers.
    #[cfg(target_os = "linux")]
    {
        assert!(tcp_fastopen_listener_supported());
        assert!(tcp_notsent_lowat_supported());
    }
    #[cfg(target_os = "macos")]
    {
        assert!(!tcp_fastopen_listener_supported());
        assert!(tcp_notsent_lowat_supported());
    }
    #[cfg(target_os = "windows")]
    {
        assert!(!tcp_fastopen_listener_supported());
        assert!(!tcp_notsent_lowat_supported());
    }
}
