#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(not(test), warn(clippy::unwrap_used))]

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
mod async_limiter;
mod limiter;
mod parse;
mod size_arg;

#[cfg(feature = "async")]
#[cfg_attr(docsrs, doc(cfg(feature = "async")))]
pub use crate::async_limiter::AsyncRateLimiter;
pub use crate::limiter::{
    BandwidthLimiter, LimiterChange, LimiterSleep, SleepBackend, active_backend,
    apply_effective_limit,
};
#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(docsrs, doc(cfg(feature = "test-support")))]
pub use crate::limiter::{RecordedSleepIter, RecordedSleepSession, recorded_sleep_session};
pub use crate::parse::{
    BandwidthLimitComponents, BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit,
};
pub use crate::size_arg::{ParsedSize, SizeArgError, parse_size_arg};
