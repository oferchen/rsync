#![deny(unsafe_code)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(missing_docs)]

//! # Overview
//!
//! `rsync_bandwidth` centralises parsing and pacing logic for rsync's
//! `--bwlimit` option. The crate exposes helpers for decoding user supplied
//! bandwidth limits together with a [`BandwidthLimiter`] state machine that
//! mirrors upstream rsync's token bucket. Higher level crates reuse these
//! utilities to share validation and throttling behaviour between the client,
//! daemon, and future transport layers.
//!
//! # Design
//!
//! - [`parse::parse_bandwidth_argument`] accepts textual rate specifications using the
//!   same syntax as upstream rsync (binary/decimal suffixes, fractional values,
//!   and optional `+1`/`-1` adjustments) and returns either an optional limit in
//!   bytes per second or a [`BandwidthParseError`].
//! - [`BandwidthLimiter`] implements the pacing algorithm used by the local copy
//!   engine and daemon. It keeps track of the accumulated byte debt and sleeps
//!   long enough to honour the configured limit while coalescing short bursts to
//!   avoid excessive context switches.
//!
//! # Invariants
//!
//! - Parsed rates are always rounded to the nearest multiple of 1024 bytes per
//!   second, matching upstream rsync.
//! - The limiter never sleeps for intervals shorter than 100ms to align with the
//!   behaviour of the C implementation.
//! - When the optional `test-support` feature is enabled (used by unit tests),
//!   sleep requests are recorded instead of reaching `std::thread::sleep`,
//!   keeping the tests deterministic and fast.
//!
//! # Examples
//!
//! Parse textual input and construct a limiter that bounds writes to 8 MiB/s.
//!
//! ```
//! use rsync_bandwidth::{parse_bandwidth_argument, BandwidthLimiter};
//! use std::num::NonZeroU64;
//!
//! let limit = parse_bandwidth_argument("8M").expect("valid limit")
//!     .expect("non-zero limit");
//! let mut limiter = BandwidthLimiter::new(limit);
//! let chunk = limiter.recommended_read_size(1 << 20);
//! assert!(chunk <= 1 << 20);
//! limiter.register(chunk);
//! ```
//!
//! # See also
//!
//! - [`rsync_core::client`](https://docs.rs/rsync-core/) and
//!   [`rsync_daemon`](https://docs.rs/rsync-daemon/) which reuse these helpers
//!   for CLI and daemon orchestration.

mod limiter;
mod parse;

pub use crate::limiter::BandwidthLimiter;
#[cfg(any(test, feature = "test-support"))]
pub use crate::limiter::{RecordedSleepSession, recorded_sleep_session};
pub use crate::parse::{
    BandwidthLimitComponents, BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit,
};
