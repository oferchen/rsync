//! Tests for `BufferRing` construction, recycling and config validation.
//!
//! The bgid allocator has its own tests in
//! [`super::allocator::tests`]; this file covers only the ring
//! lifecycle, the kernel registration path and the
//! [`crate::io_uring_common`] re-exports.

use super::*;
use crate::io_uring_common::{IORING_CQE_BUFFER_SHIFT, IORING_CQE_F_BUFFER};

#[test]
fn config_default_has_valid_values() {
    let config = BufferRingConfig::default();
    assert!(config.ring_size.is_power_of_two());
    assert!(config.ring_size > 0);
    assert!(config.buffer_size > 0);
    assert_eq!(config.bgid, 0);
}

#[test]
fn config_validate_rejects_zero_ring_size() {
    let config = BufferRingConfig {
        ring_size: 0,
        ..Default::default()
    };
    assert!(validate_buffer_ring_config(&config).is_err());
}

#[test]
fn config_validate_rejects_non_power_of_two() {
    let config = BufferRingConfig {
        ring_size: 3,
        ..Default::default()
    };
    assert!(validate_buffer_ring_config(&config).is_err());
}

#[test]
fn config_validate_rejects_zero_buffer_size() {
    let config = BufferRingConfig {
        buffer_size: 0,
        ..Default::default()
    };
    assert!(validate_buffer_ring_config(&config).is_err());
}

#[test]
fn config_validate_accepts_valid_config() {
    let config = BufferRingConfig {
        ring_size: 16,
        buffer_size: 4096,
        bgid: 1,
    };
    assert!(validate_buffer_ring_config(&config).is_ok());
}

#[test]
fn config_validate_accepts_large_power_of_two() {
    let config = BufferRingConfig {
        ring_size: 1024,
        buffer_size: 256 * 1024,
        bgid: 0,
    };
    assert!(validate_buffer_ring_config(&config).is_ok());
}

#[test]
fn is_supported_returns_bool_without_panic() {
    // On any platform, is_supported must not panic. It returns true on
    // Linux >= 5.19 and false otherwise.
    let _result: bool = is_supported();
}

#[test]
fn buffer_id_from_cqe_flags_extracts_id() {
    // Buffer ID 42 encoded in upper 16 bits with IORING_CQE_F_BUFFER set.
    let flags = (42u32 << IORING_CQE_BUFFER_SHIFT) | IORING_CQE_F_BUFFER;
    assert_eq!(buffer_id_from_cqe_flags(flags), Some(42));
}

#[test]
fn buffer_id_from_cqe_flags_returns_none_without_flag() {
    // No IORING_CQE_F_BUFFER flag set.
    let flags = 42u32 << IORING_CQE_BUFFER_SHIFT;
    assert_eq!(buffer_id_from_cqe_flags(flags), None);
}

#[test]
fn buffer_id_from_cqe_flags_zero_id() {
    let flags = IORING_CQE_F_BUFFER; // buffer ID = 0
    assert_eq!(buffer_id_from_cqe_flags(flags), Some(0));
}

#[test]
fn buffer_id_from_cqe_flags_max_id() {
    let flags = (u16::MAX as u32) << IORING_CQE_BUFFER_SHIFT | IORING_CQE_F_BUFFER;
    assert_eq!(buffer_id_from_cqe_flags(flags), Some(u16::MAX));
}

#[test]
fn buffer_ring_error_converts_to_io_error() {
    let err: io::Error = BufferRingError::KernelTooOld {
        major: 5,
        minor: 15,
    }
    .into();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);

    let err: io::Error = BufferRingError::InvalidRingSize(3).into();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);

    let err: io::Error = BufferRingError::InvalidBufferSize.into();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn buffer_ring_error_display_messages() {
    let err = BufferRingError::KernelTooOld {
        major: 5,
        minor: 15,
    };
    let msg = format!("{err}");
    assert!(msg.contains("5.19"));
    assert!(msg.contains("5.15"));

    let err = BufferRingError::InvalidRingSize(7);
    let msg = format!("{err}");
    assert!(msg.contains("power of 2"));
    assert!(msg.contains("7"));
}

#[test]
fn buffer_ring_new_on_supported_kernel() {
    // Skip if io_uring is not available or kernel < 5.19.
    if !is_supported() {
        return;
    }
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };

    let config = BufferRingConfig {
        ring_size: 4,
        buffer_size: 4096,
        bgid: 0,
    };

    let buf_ring = match BufferRing::new(&ring, config) {
        Ok(br) => br,
        Err(_) => return, // May fail due to seccomp or permissions
    };

    assert_eq!(buf_ring.ring_size(), 4);
    assert_eq!(buf_ring.buffer_size(), 4096);
    assert_eq!(buf_ring.bgid(), 0);

    // Verify buffer pointers are valid and in-range.
    for i in 0..4u16 {
        let ptr = buf_ring.buffer_ptr(i);
        assert!(ptr.is_some(), "buffer {i} pointer should be valid");
        assert!(
            !ptr.unwrap().is_null(),
            "buffer {i} pointer should not be null"
        );
    }

    // Out-of-range buffer ID.
    assert!(buf_ring.buffer_ptr(4).is_none());
    assert!(buf_ring.buffer_ptr(u16::MAX).is_none());

    // Drop triggers cleanup (unregister, munmap, dealloc).
    drop(buf_ring);
}

#[test]
fn buffer_ring_try_new_returns_none_on_failure() {
    // On kernels < 5.19 or without io_uring, try_new should return None.
    if is_supported() {
        return; // Skip - we need a failing case for this test
    }

    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => {
            // io_uring itself unavailable - try_new should also fail
            // but we cannot even create the ring. Verify is_supported is false.
            assert!(!is_supported());
            return;
        }
    };

    let config = BufferRingConfig::default();
    assert!(BufferRing::try_new(&ring, config).is_none());
}

#[test]
fn buffer_ring_recycle_on_supported_kernel() {
    if !is_supported() {
        return;
    }
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };

    let config = BufferRingConfig {
        ring_size: 4,
        buffer_size: 4096,
        bgid: 1,
    };

    let buf_ring = match BufferRing::new(&ring, config) {
        Ok(br) => br,
        Err(_) => return,
    };

    // Recycling each buffer in range must succeed.
    buf_ring.recycle_buffer(0).expect("recycle 0");
    buf_ring.recycle_buffer(1).expect("recycle 1");
    buf_ring.recycle_buffer(2).expect("recycle 2");
    buf_ring.recycle_buffer(3).expect("recycle 3");

    drop(buf_ring);
}

#[test]
fn buffer_ring_recycle_rejects_out_of_range_buf_id() {
    if !is_supported() {
        return;
    }
    let ring = match RawIoUring::new(4) {
        Ok(r) => r,
        Err(_) => return,
    };

    let config = BufferRingConfig {
        ring_size: 4,
        buffer_size: 4096,
        bgid: 2,
    };

    let buf_ring = match BufferRing::new(&ring, config) {
        Ok(br) => br,
        Err(_) => return,
    };

    // First out-of-range id is ring_size; this must be rejected without
    // mutating the shared ring tail or panicking.
    match buf_ring.recycle_buffer(4) {
        Err(BufferRingError::BufferIdOutOfRange { buf_id, ring_size }) => {
            assert_eq!(buf_id, 4);
            assert_eq!(ring_size, 4);
        }
        other => panic!("expected BufferIdOutOfRange, got {other:?}"),
    }

    // Far-out-of-range id (u16::MAX) must also be rejected.
    assert!(matches!(
        buf_ring.recycle_buffer(u16::MAX),
        Err(BufferRingError::BufferIdOutOfRange { .. })
    ));

    drop(buf_ring);
}

#[test]
fn buffer_ring_error_out_of_range_converts_to_invalid_input() {
    let err: io::Error = BufferRingError::BufferIdOutOfRange {
        buf_id: 9,
        ring_size: 4,
    }
    .into();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    let msg = format!("{err}");
    assert!(msg.contains("buf_id 9"));
    assert!(msg.contains("ring size 4"));
}

#[test]
fn page_size_is_positive_and_power_of_two() {
    let ps = page_size();
    assert!(ps > 0);
    assert!(ps.is_power_of_two());
}
