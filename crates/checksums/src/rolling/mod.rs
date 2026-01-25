//! Rolling checksum implementation for rsync delta transfers.
//!
//! The rolling checksum is a weak but fast checksum used to identify candidate
//! blocks during delta transfers. It allows efficient sliding window computation
//! where updating the checksum for a shifted window requires O(1) operations
//! rather than recomputing from scratch.
//!
//! # Algorithm
//!
//! This module implements the Adler-32–style rolling checksum used by rsync,
//! which maintains two 16-bit components (a simple sum and a weighted sum)
//! that can be incrementally updated as the window slides over data.
//!
//! # SIMD Acceleration
//!
//! On supported platforms (x86_64 with AVX2/SSE4.1, aarch64 with NEON), the
//! bulk update operations use SIMD instructions for improved throughput.
//! Use [`simd_acceleration_available`] to query runtime SIMD support.
//!
//! # Example
//!
//! ```rust
//! use checksums::RollingChecksum;
//!
//! let mut rolling = RollingChecksum::new();
//! rolling.update(b"hello");
//!
//! // Slide window: remove 'h', add '!'
//! rolling.roll(b'h', b'!').unwrap();
//! ```

/// Macro to implement From trait for both owned and reference types.
///
/// This reduces boilerplate when implementing conversions that work
/// identically for both `T` and `&T`. The macro generates both
/// implementations, ensuring consistent behavior and eliminating
/// duplicate code.
///
/// # Arguments
///
/// * `$source` - Source type (will also generate `&$source`)
/// * `$target` - Target type
/// * `$method` - Method to call on the source for conversion
macro_rules! impl_from_owned_and_ref {
    ($source:ty => $target:ty, $method:ident) => {
        impl From<$source> for $target {
            #[inline]
            fn from(value: $source) -> Self {
                value.$method()
            }
        }

        impl From<&$source> for $target {
            #[inline]
            fn from(value: &$source) -> Self {
                value.$method()
            }
        }
    };
}

mod checksum;
mod digest;
mod error;

pub use checksum::{RollingChecksum, simd_acceleration_available};
pub use digest::RollingDigest;
pub use error::{RollingError, RollingSliceError};

#[cfg(test)]
mod tests;
