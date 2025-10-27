#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod limiter;
mod parse;

pub use crate::limiter::{BandwidthLimiter, LimiterChange, apply_effective_limit};
#[cfg(any(test, feature = "test-support"))]
pub use crate::limiter::{RecordedSleepSession, recorded_sleep_session};
pub use crate::parse::{
    BandwidthLimitComponents, BandwidthParseError, parse_bandwidth_argument, parse_bandwidth_limit,
};
