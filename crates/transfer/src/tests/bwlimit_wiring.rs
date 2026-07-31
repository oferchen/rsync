//! Tests for the sender-role gating of `--bwlimit` on the server transfer body.
//!
//! These encode the upstream invariant that only the sender paces its own
//! outbound socket writes: `main.c:1068` disables the receiver's throttle
//! (`bwlimit_writemax = 0`), while the sender clamps and sleeps per write
//! (`io.c:846,861`). The wiring carries the effective rate on
//! `ConnectionConfig::bwlimit`; [`crate::sender_bandwidth_limiter`] turns it
//! into a live limiter only for the Generator (sender) role.

use std::num::NonZeroU64;

use bandwidth::BandwidthLimitComponents;

use crate::sender_bandwidth_limiter;
use crate::{ServerConfig, ServerRole};

fn config_with_bwlimit(
    role: ServerRole,
    bwlimit: Option<BandwidthLimitComponents>,
) -> ServerConfig {
    let mut config =
        ServerConfig::from_flag_string_and_args(role, "-logDtpre.iLsfxC".to_owned(), Vec::new())
            .expect("config parses");
    config.connection.bwlimit = bwlimit;
    config
}

fn components(rate: u64) -> BandwidthLimitComponents {
    BandwidthLimitComponents::new(NonZeroU64::new(rate), None)
}

/// The sender (Generator) installs a limiter when a non-zero `--bwlimit` is set.
///
/// WHY: the sender must pace its socket writes (upstream `io.c:846,861`), so a
/// configured rate on the Generator role must materialise a live limiter whose
/// rate matches - otherwise `--bwlimit` is inert on the network path, the exact
/// pre-fix bug.
#[test]
fn generator_with_bwlimit_builds_limiter() {
    let config = config_with_bwlimit(ServerRole::Generator, Some(components(64 * 1024)));
    let limiter = sender_bandwidth_limiter(&config).expect("sender must throttle");
    assert_eq!(limiter.limit_bytes().get(), 64 * 1024);
}

/// The receiver never throttles, even when a `--bwlimit` rate is present.
///
/// WHY: upstream `main.c:1068` sets `bwlimit_writemax = 0` on the receiver, so
/// its writes (of ACKs / file-list requests) are never paced. Forwarding a
/// `--bwlimit` value to a receiver (upstream always forwards it) must remain a
/// no-op on its writer.
#[test]
fn receiver_never_throttles() {
    let config = config_with_bwlimit(ServerRole::Receiver, Some(components(64 * 1024)));
    assert!(
        sender_bandwidth_limiter(&config).is_none(),
        "receiver must not pace its writes (main.c:1068)"
    );
}

/// A generator with no `--bwlimit` (or a zero rate) is a passthrough.
///
/// WHY: `--bwlimit=0` means unlimited; the sender's writer must stay a
/// zero-overhead passthrough so the default network path is unperturbed.
#[test]
fn generator_without_bwlimit_is_passthrough() {
    let none = config_with_bwlimit(ServerRole::Generator, None);
    assert!(sender_bandwidth_limiter(&none).is_none());

    let zero = config_with_bwlimit(ServerRole::Generator, Some(components(0)));
    assert!(
        sender_bandwidth_limiter(&zero).is_none(),
        "a zero rate is unlimited, not a throttle"
    );
}

/// The configured burst is carried into the limiter for the sender.
///
/// WHY: the daemon module `bwlimit RATE:BURST` directive and the client's
/// burst component must reach the live limiter so debt clamping matches the
/// configured burst, not just the steady-state rate.
#[test]
fn generator_preserves_burst() {
    let comps = BandwidthLimitComponents::new_with_specified(
        NonZeroU64::new(64 * 1024),
        NonZeroU64::new(8192),
        true,
    );
    let config = config_with_bwlimit(ServerRole::Generator, Some(comps));
    let limiter = sender_bandwidth_limiter(&config).expect("sender must throttle");
    assert_eq!(limiter.limit_bytes().get(), 64 * 1024);
    assert_eq!(limiter.burst_bytes().map(NonZeroU64::get), Some(8192));
}
