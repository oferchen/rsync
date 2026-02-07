//! Memory limits tests for max_alloc configuration.
//!
//! These tests verify the `--max-alloc` configuration option works correctly,
//! including storage, retrieval, and typical usage patterns.

use core::client::ClientConfigBuilder;

// ============================================================================
// Max-Alloc Configuration Tests
// ============================================================================

/// Tests that max_alloc configuration is properly stored and retrievable.
#[test]
fn max_alloc_config_builder_stores_value() {
    let limit = 1024 * 1024 * 1024u64; // 1 GB
    let config = ClientConfigBuilder::default()
        .max_alloc(Some(limit))
        .build();

    assert_eq!(config.max_alloc(), Some(limit));
}

#[test]
fn max_alloc_config_none_by_default() {
    let config = ClientConfigBuilder::default().build();
    assert!(config.max_alloc().is_none());
}

#[test]
fn max_alloc_config_can_be_cleared() {
    let config = ClientConfigBuilder::default()
        .max_alloc(Some(1024 * 1024))
        .max_alloc(None)
        .build();

    assert!(config.max_alloc().is_none());
}

#[test]
fn max_alloc_config_common_values() {
    // Test common --max-alloc values
    let test_cases = [
        ("128M", 128 * 1024 * 1024u64),
        ("256M", 256 * 1024 * 1024u64),
        ("512M", 512 * 1024 * 1024u64),
        ("1G", 1024 * 1024 * 1024u64),
        ("2G", 2 * 1024 * 1024 * 1024u64),
    ];

    for (_label, value) in test_cases {
        let config = ClientConfigBuilder::default()
            .max_alloc(Some(value))
            .build();

        assert_eq!(
            config.max_alloc(),
            Some(value),
            "max_alloc should store {value}"
        );
    }
}

#[test]
fn max_alloc_config_small_values() {
    // Very small limits (defensive against DoS)
    let test_cases = [
        1024u64,        // 1 KB
        4096u64,        // 4 KB
        64 * 1024u64,   // 64 KB
        1024 * 1024u64, // 1 MB
    ];

    for value in test_cases {
        let config = ClientConfigBuilder::default()
            .max_alloc(Some(value))
            .build();

        assert_eq!(config.max_alloc(), Some(value));
    }
}

#[test]
fn max_alloc_config_large_values() {
    // Very large limits (for high-memory systems)
    let test_cases = [
        4 * 1024 * 1024 * 1024u64,  // 4 GB
        8 * 1024 * 1024 * 1024u64,  // 8 GB
        16 * 1024 * 1024 * 1024u64, // 16 GB
    ];

    for value in test_cases {
        let config = ClientConfigBuilder::default()
            .max_alloc(Some(value))
            .build();

        assert_eq!(config.max_alloc(), Some(value));
    }
}

#[test]
fn max_alloc_config_maximum_value() {
    // Test with u64::MAX (no effective limit)
    let config = ClientConfigBuilder::default()
        .max_alloc(Some(u64::MAX))
        .build();

    assert_eq!(config.max_alloc(), Some(u64::MAX));
}

#[test]
fn max_alloc_config_zero_value() {
    // Zero is a valid value (though not useful in practice)
    let config = ClientConfigBuilder::default().max_alloc(Some(0)).build();

    assert_eq!(config.max_alloc(), Some(0));
}

// ============================================================================
// Memory Limit Enforcement Simulation
// ============================================================================

/// Simulates checking an allocation against a max_alloc limit.
fn check_allocation_limit(requested: u64, max_alloc: Option<u64>) -> bool {
    match max_alloc {
        Some(limit) => requested <= limit,
        None => true, // No limit
    }
}

#[test]
fn memory_limit_allows_within_bounds() {
    let limit = 1024 * 1024u64; // 1 MB

    assert!(check_allocation_limit(512 * 1024, Some(limit)));
    assert!(check_allocation_limit(1024 * 1024, Some(limit)));
}

#[test]
fn memory_limit_denies_over_limit() {
    let limit = 1024 * 1024u64; // 1 MB

    assert!(!check_allocation_limit(1024 * 1024 + 1, Some(limit)));
    assert!(!check_allocation_limit(2 * 1024 * 1024, Some(limit)));
}

#[test]
fn memory_limit_allows_all_when_none() {
    assert!(check_allocation_limit(u64::MAX, None));
}

#[test]
fn memory_limit_typical_values() {
    // Test with typical rsync max-alloc values
    let values = [
        128 * 1024 * 1024u64,  // 128M
        256 * 1024 * 1024u64,  // 256M
        512 * 1024 * 1024u64,  // 512M
        1024 * 1024 * 1024u64, // 1G
    ];

    for limit in values {
        // Should allow half the limit
        assert!(check_allocation_limit(limit / 2, Some(limit)));
        // Should deny double the limit
        assert!(!check_allocation_limit(limit * 2, Some(limit)));
    }
}

// ============================================================================
// Edge Cases
// ============================================================================

#[test]
fn memory_limit_boundary_exact_match() {
    let limit = 1024 * 1024u64;

    // Exactly at limit should be allowed
    assert!(check_allocation_limit(limit, Some(limit)));
}

#[test]
fn memory_limit_boundary_one_over() {
    let limit = 1024 * 1024u64;

    // One byte over limit should be denied
    assert!(!check_allocation_limit(limit + 1, Some(limit)));
}

#[test]
fn memory_limit_zero_allocation() {
    let limit = 1024 * 1024u64;

    // Zero allocation is always allowed
    assert!(check_allocation_limit(0, Some(limit)));
    assert!(check_allocation_limit(0, None));
}

#[test]
fn memory_limit_zero_limit() {
    // With a zero limit, only zero allocation is allowed
    assert!(check_allocation_limit(0, Some(0)));
    assert!(!check_allocation_limit(1, Some(0)));
}
