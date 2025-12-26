#![doc = include_str!("../README.md")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod rolling;
pub mod strong;

#[cfg(feature = "parallel")]
#[cfg_attr(docsrs, doc(cfg(feature = "parallel")))]
pub mod parallel;

pub use rolling::{
    RollingChecksum, RollingDigest, RollingError, RollingSliceError, simd_acceleration_available,
};
pub use strong::openssl_acceleration_available;
