//! Congestion control and flow-control window tuning for the QUIC transport.
//!
//! Both endpoints - the client [`ClientConfig`](quinn_proto::ClientConfig) and
//! the server [`ServerConfig`](quinn_proto::ServerConfig) - attach the single
//! [`TransportConfig`] built by [`build_transport_config`], so the two
//! construction sites stay in lockstep. quinn-proto's stock `EndpointConfig`
//! leaves the connection on its default NewReno controller with a ~1.25 MiB
//! stream window, which is both congestion-limited and window-limited on
//! high-bandwidth-delay-product links.
//!
//! # Wire neutrality
//!
//! Congestion control and flow-control windows govern only pacing and
//! back-pressure: how fast bytes leave and how many may be in flight before an
//! acknowledgement. They never change a wire byte or any protocol semantic, so
//! selecting a different controller or window size is observable only as
//! throughput and latency, never as a compatibility difference. A peer running
//! any setting interoperates with a peer running any other.
//!
//! # Tunables
//!
//! - `OC_RSYNC_QUIC_CC` selects the congestion controller: `bbr`, `cubic`, or
//!   `newreno`. Default `bbr` (the high-BDP fit). An unrecognized value is a
//!   hard error rather than a silent fallback.
//! - `OC_RSYNC_QUIC_WINDOW` sizes the flow-control windows in bytes, accepting
//!   an optional `K`/`M`/`G` binary (1024-based) suffix. Default
//!   [`DEFAULT_WINDOW`].

use std::io;
use std::sync::Arc;

use quinn_proto::congestion::{BbrConfig, ControllerFactory, CubicConfig, NewRenoConfig};
use quinn_proto::{TransportConfig, VarInt};

/// Environment variable selecting the congestion controller.
const CC_ENV: &str = "OC_RSYNC_QUIC_CC";
/// Environment variable sizing the flow-control windows.
const WINDOW_ENV: &str = "OC_RSYNC_QUIC_WINDOW";

/// Default flow-control window when `OC_RSYNC_QUIC_WINDOW` is unset: 64 MiB.
///
/// The window must cover the bandwidth-delay product (BDP) of the link, or the
/// sender stalls waiting for acknowledgements before the pipe is full. 64 MiB
/// saturates a single stream up to roughly 5 Gbit/s at a 100 ms RTT
/// (`64 MiB / 0.1 s ~= 640 MB/s`), a generous envelope that still bounds
/// per-connection receive memory. It deliberately overshoots quinn-proto's
/// ~1.25 MiB default, which caps a 100 ms-RTT stream near 100 Mbit/s.
///
/// This is a defensible starting point, not a measured optimum: the ideal
/// window is `bandwidth * RTT` for the target link, and the repo's
/// measurement-gate discipline (ARCH-D3) requires confirming it with a real
/// loopback-plus-delay throughput measurement before treating any specific
/// value as tuned. Operators on an atypical link override it via
/// `OC_RSYNC_QUIC_WINDOW`.
pub(super) const DEFAULT_WINDOW: u64 = 64 * 1024 * 1024;

/// Selectable QUIC congestion-control algorithm.
///
/// The `quinn-proto` factory types are opaque trait objects that cannot be
/// compared, so parsing resolves to this enum first; [`Self::factory`] then
/// maps it to the controller factory. This keeps the string-to-algorithm
/// mapping unit-testable independently of the transport. Public so the CLI
/// (`--quic-cc`) and core can carry a resolved choice down to
/// [`build_transport_config`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CongestionAlgorithm {
    /// BBR - model-based, the high-BDP default.
    Bbr,
    /// CUBIC - loss-based, the modern TCP default.
    Cubic,
    /// NewReno - loss-based, the QUIC baseline (quinn-proto's own default).
    NewReno,
}

impl CongestionAlgorithm {
    /// The accepted controller names, in a stable order for CLI value
    /// restriction and help text (`--quic-cc <bbr|cubic|newreno>`).
    pub const NAMES: [&'static str; 3] = ["bbr", "cubic", "newreno"];

    /// Parses a controller name (`OC_RSYNC_QUIC_CC` or `--quic-cc`), failing
    /// loudly on anything but the three recognized names (repo policy: never
    /// silently fall back on a bad argument).
    pub fn parse(value: &str) -> io::Result<Self> {
        match value.trim() {
            "bbr" => Ok(Self::Bbr),
            "cubic" => Ok(Self::Cubic),
            "newreno" => Ok(Self::NewReno),
            other => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "unrecognized QUIC congestion controller {other:?}; expected one of: bbr, cubic, newreno"
                ),
            )),
        }
    }

    /// Reads `OC_RSYNC_QUIC_CC`, defaulting to [`Self::Bbr`] when it is unset.
    fn from_env() -> io::Result<Self> {
        match std::env::var(CC_ENV) {
            Ok(value) => Self::parse(&value),
            Err(_) => Ok(Self::Bbr),
        }
    }

    /// Builds the `quinn-proto` congestion-controller factory for this
    /// algorithm.
    fn factory(self) -> Arc<dyn ControllerFactory + Send + Sync + 'static> {
        match self {
            Self::Bbr => Arc::new(BbrConfig::default()),
            Self::Cubic => Arc::new(CubicConfig::default()),
            Self::NewReno => Arc::new(NewRenoConfig::default()),
        }
    }
}

/// Parses a flow-control window size: a `u64` byte count with an optional
/// single-letter binary suffix (`K`, `M`, or `G`, case-insensitive, 1024-based).
pub(super) fn parse_window(value: &str) -> io::Result<u64> {
    let trimmed = value.trim();
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "invalid {WINDOW_ENV} value {trimmed:?}; expected a byte count \
                 with an optional K, M, or G suffix"
            ),
        )
    };
    let (digits, multiplier): (&str, u64) = match trimmed.as_bytes().last() {
        Some(&last) if last.is_ascii_alphabetic() => {
            let factor = match last.to_ascii_lowercase() {
                b'k' => 1024,
                b'm' => 1024 * 1024,
                b'g' => 1024 * 1024 * 1024,
                _ => return Err(invalid()),
            };
            (&trimmed[..trimmed.len() - 1], factor)
        }
        _ => (trimmed, 1),
    };
    let base: u64 = digits.trim().parse().map_err(|_| invalid())?;
    base.checked_mul(multiplier).ok_or_else(invalid)
}

/// Reads `OC_RSYNC_QUIC_WINDOW`, defaulting to [`DEFAULT_WINDOW`] when unset.
fn window_from_env() -> io::Result<u64> {
    match std::env::var(WINDOW_ENV) {
        Ok(value) => parse_window(&value),
        Err(_) => Ok(DEFAULT_WINDOW),
    }
}

/// Explicit transport-tuning overrides supplied by the CLI (`--quic-cc`,
/// `--quic-window`), each taking precedence over the environment fallback.
///
/// A `None` field defers to the environment variable and then the built-in
/// default, so the resolution order is CLI flag > environment variable >
/// default, all resolved in the single [`build_transport_config`] owner.
/// [`Default`] (both fields `None`) is the env/default-only path used by the
/// daemon's own endpoint, which no client CLI flag configures.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuicTransportTuning {
    /// `--quic-cc`: the congestion controller, or `None` to consult
    /// `OC_RSYNC_QUIC_CC` then default to BBR.
    pub congestion: Option<CongestionAlgorithm>,
    /// `--quic-window`: the flow-control window in bytes, or `None` to consult
    /// `OC_RSYNC_QUIC_WINDOW` then default to [`DEFAULT_WINDOW`].
    pub window: Option<u64>,
}

/// Builds the shared [`TransportConfig`] consulted by both the client and the
/// server construction sites.
///
/// Resolution order, applied here so there is a single resolution site:
/// CLI override (`tuning`) > environment variable (`OC_RSYNC_QUIC_CC` /
/// `OC_RSYNC_QUIC_WINDOW`) > built-in default (BBR / [`DEFAULT_WINDOW`]). The
/// per-stream and connection receive windows and the send window are all set to
/// the same value; quinn requires `receive_window >= stream_receive_window`,
/// which equality satisfies, and rsync drives a single bidirectional stream so
/// the per-stream window is the binding constraint. This function is
/// behaviour-neutral on the wire - it changes only pacing and back-pressure
/// (see the module docs).
pub(super) fn build_transport_config(
    tuning: QuicTransportTuning,
) -> io::Result<Arc<TransportConfig>> {
    let algorithm = match tuning.congestion {
        Some(algorithm) => algorithm,
        None => CongestionAlgorithm::from_env()?,
    };
    let window = match tuning.window {
        Some(window) => window,
        None => window_from_env()?,
    };
    let stream_window = VarInt::from_u64(window).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{WINDOW_ENV} value {window} exceeds the QUIC VarInt maximum"),
        )
    })?;

    let mut config = TransportConfig::default();
    config
        .congestion_controller_factory(algorithm.factory())
        .stream_receive_window(stream_window)
        .receive_window(stream_window)
        .send_window(window);
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These unit tests stay pure: `rsync_io` is `#![deny(unsafe_code)]` and
    // `std::env::set_var` is `unsafe`, so env-driven selection is exercised by
    // the guarded loopback test (`round_trip_under_each_congestion_controller`
    // in tests/quic_loopback.rs, a separate crate where env mutation is
    // permitted). Here we test the parsing and the builder's default path.

    /// Each recognized name resolves to its own algorithm; surrounding
    /// whitespace is tolerated.
    #[test]
    fn parse_maps_each_name_to_its_algorithm() {
        assert_eq!(
            CongestionAlgorithm::parse("bbr").expect("bbr"),
            CongestionAlgorithm::Bbr
        );
        assert_eq!(
            CongestionAlgorithm::parse("cubic").expect("cubic"),
            CongestionAlgorithm::Cubic
        );
        assert_eq!(
            CongestionAlgorithm::parse("newreno").expect("newreno"),
            CongestionAlgorithm::NewReno
        );
        assert_eq!(
            CongestionAlgorithm::parse("  bbr  ").expect("padded bbr"),
            CongestionAlgorithm::Bbr
        );
    }

    /// An unrecognized controller name fails loudly instead of falling back.
    #[test]
    fn parse_rejects_unknown_algorithm() {
        let err = CongestionAlgorithm::parse("reno").expect_err("must reject");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    /// The window parser accepts plain byte counts and binary suffixes, and
    /// rejects garbage and overflow.
    #[test]
    fn parse_window_handles_suffixes_and_errors() {
        assert_eq!(parse_window("1024").expect("plain"), 1024);
        assert_eq!(parse_window("8K").expect("K"), 8 * 1024);
        assert_eq!(parse_window("2m").expect("m"), 2 * 1024 * 1024);
        assert_eq!(parse_window("1G").expect("G"), 1024 * 1024 * 1024);
        parse_window("").expect_err("empty");
        parse_window("12T").expect_err("unknown suffix");
        parse_window("abc").expect_err("non-numeric");
        parse_window(&format!("{}G", u64::MAX)).expect_err("overflow");
    }

    /// The builder constructs a transport config under the default environment
    /// (no `OC_RSYNC_*` set): the default controller (BBR) and the default
    /// window feed a valid config without error.
    #[test]
    fn builder_succeeds_under_default_env() {
        build_transport_config(QuicTransportTuning::default()).expect("default transport config");
    }

    /// Explicit CLI-style overrides build a valid config without consulting the
    /// environment. (Env-vs-CLI precedence is exercised end-to-end by the
    /// loopback test, where env mutation is permitted.)
    #[test]
    fn builder_accepts_explicit_overrides() {
        let tuning = QuicTransportTuning {
            congestion: Some(CongestionAlgorithm::Cubic),
            window: Some(8 * 1024 * 1024),
        };
        build_transport_config(tuning).expect("override transport config");
    }
}
