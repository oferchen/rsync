#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod rolling;
pub mod strong;

pub use rolling::{RollingChecksum, RollingDigest, RollingError, RollingSliceError};
