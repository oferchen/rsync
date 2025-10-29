mod checksum;
mod digest;
mod error;

pub use checksum::RollingChecksum;
pub use digest::RollingDigest;
pub use error::{RollingError, RollingSliceError};

#[cfg(test)]
mod tests;
