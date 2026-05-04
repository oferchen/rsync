//! Protocol version interoperability matrix tests.
//!
//! Validates that all supported protocol versions can successfully negotiate
//! with each other and that feature availability is correctly determined by
//! the negotiated version.

use protocol::{ProtocolVersionAdvertisement, select_highest_mutual};

/// Helper wrapper to implement ProtocolVersionAdvertisement for testing.
#[derive(Clone, Copy, Debug)]
struct TestVersion(u32);

impl ProtocolVersionAdvertisement for TestVersion {
    #[inline]
    fn into_advertised_version(self) -> u32 {
        self.0
    }
}

/// All protocol versions supported by oc-rsync.
const SUPPORTED_VERSIONS: [u8; 5] = [28, 29, 30, 31, 32];

#[test]
fn test_protocol_matrix_all_supported_combinations() {
    // Test every supported protocol version
    // select_highest_mutual checks if any of the client's advertised versions
    // are supported by us (versions 28-32), and returns the highest one
    for &client_version in &SUPPORTED_VERSIONS {
        let client_advertises = [TestVersion(u32::from(client_version))];

        let result = select_highest_mutual(client_advertises);

        // Client advertises a supported version, so negotiation should succeed
        assert!(
            result.is_ok(),
            "Negotiation failed for client advertised version {client_version}"
        );

        let negotiated = result.unwrap();

        // The negotiated version should be the client's advertised version
        // (since we support all versions 28-32)
        assert_eq!(
            negotiated.as_u8(),
            client_version,
            "Expected protocol {client_version} when client advertises {client_version}"
        );
    }
}

#[test]
fn test_protocol_matrix_client_newer_than_server() {
    // Client supports newer protocol than server
    // Server only supports up to protocol 29
    let client_advertises = [TestVersion(32)];

    // In a real scenario, the server would advertise only its supported versions
    // For this test, we simulate negotiation by selecting the highest mutual version
    // Since our select_highest_mutual checks against our own supported versions (28-32),
    // this will succeed. In practice, protocol negotiation would compare against
    // the server's advertised versions.

    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_u8(), 32);
}

#[test]
fn test_protocol_matrix_server_newer_than_client() {
    // Server supports newer protocol than client advertises
    // Client only advertises protocol 28
    let client_advertises = [TestVersion(28)];

    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_u8(), 28);
}

#[test]
fn test_protocol_matrix_multiple_client_versions() {
    // Client advertises multiple versions (typical scenario)
    let client_advertises = [
        TestVersion(32),
        TestVersion(31),
        TestVersion(30),
        TestVersion(29),
        TestVersion(28),
    ];

    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());

    // Should negotiate highest version (32)
    assert_eq!(result.unwrap().as_u8(), 32);
}

#[test]
fn test_protocol_matrix_sparse_client_versions() {
    // Client advertises non-consecutive versions
    let client_advertises = [TestVersion(32), TestVersion(30), TestVersion(28)];

    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());

    // Should negotiate highest (32)
    assert_eq!(result.unwrap().as_u8(), 32);
}

#[test]
fn test_protocol_matrix_legacy_negotiation_boundary() {
    // Test the boundary between ASCII (28-29) and binary (30+) negotiation

    // Protocol 29 uses ASCII negotiation
    let client_29 = [TestVersion(29)];
    let result = select_highest_mutual(client_29);
    assert!(result.is_ok());
    let protocol_29 = result.unwrap();
    assert_eq!(protocol_29.as_u8(), 29);
    assert!(protocol_29.uses_legacy_ascii_negotiation());
    assert!(!protocol_29.uses_binary_negotiation());

    // Protocol 30 uses binary negotiation
    let client_30 = [TestVersion(30)];
    let result = select_highest_mutual(client_30);
    assert!(result.is_ok());
    let protocol_30 = result.unwrap();
    assert_eq!(protocol_30.as_u8(), 30);
    assert!(!protocol_30.uses_legacy_ascii_negotiation());
    assert!(protocol_30.uses_binary_negotiation());
}

#[test]
fn test_protocol_matrix_feature_availability_by_version() {
    // Verify that features are correctly gated by protocol version
    for &version in &SUPPORTED_VERSIONS {
        let client_advertises = [TestVersion(u32::from(version))];
        let result = select_highest_mutual(client_advertises);
        assert!(result.is_ok());

        let protocol = result.unwrap();

        // Binary negotiation available in protocol 30+
        if version >= 30 {
            assert!(
                protocol.uses_binary_negotiation(),
                "Protocol {version} should use binary negotiation"
            );
        } else {
            assert!(
                protocol.uses_legacy_ascii_negotiation(),
                "Protocol {version} should use ASCII negotiation"
            );
        }

        // All versions should support basic rsync operations
        // (This is implicit in successful negotiation)
        assert!(protocol.as_u8() >= 28 && protocol.as_u8() <= 32);
    }
}

#[test]
fn test_protocol_matrix_minimum_version_28() {
    // Verify we support down to protocol 28
    let client_advertises = [TestVersion(28)];
    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_u8(), 28);
}

#[test]
fn test_protocol_matrix_maximum_version_32() {
    // Verify we support up to protocol 32
    let client_advertises = [TestVersion(32)];
    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_u8(), 32);
}

#[test]
fn test_protocol_matrix_version_27_rejected() {
    // Protocol 27 is too old and should be rejected
    let client_advertises = [TestVersion(27)];
    let result = select_highest_mutual(client_advertises);
    assert!(
        result.is_err(),
        "Protocol 27 should be rejected as unsupported"
    );
}

#[test]
fn test_protocol_matrix_version_33_clamped() {
    // Protocol 33 is above our range but below MAX_ADVERTISEMENT (40)
    // so it should be clamped to 32
    let client_advertises = [TestVersion(33)];
    let result = select_highest_mutual(client_advertises);
    assert!(
        result.is_ok(),
        "Protocol 33 should be clamped to 32, not rejected"
    );
    assert_eq!(
        result.unwrap().as_u8(),
        32,
        "Protocol 33 should be clamped to protocol 32"
    );
}

#[test]
fn test_protocol_matrix_version_41_rejected() {
    // Protocol 41 is above MAX_ADVERTISEMENT (40) and should be rejected
    let client_advertises = [TestVersion(41)];
    let result = select_highest_mutual(client_advertises);
    assert!(result.is_err(), "Protocol 41 should be rejected as too new");
}

#[test]
fn test_protocol_matrix_version_range_validation() {
    // Test boundary conditions
    // MAXIMUM_PROTOCOL_ADVERTISEMENT is 40 in the protocol crate
    const MAX_ADVERTISEMENT: u32 = 40;

    for version in 0..100u32 {
        let client_advertises = [TestVersion(version)];
        let result = select_highest_mutual(client_advertises);

        if (28..=32).contains(&version) {
            // Versions in our supported range: accept as-is
            assert!(result.is_ok(), "Protocol {version} should be supported");
            assert_eq!(result.unwrap().as_u8(), version as u8);
        } else if (33..=MAX_ADVERTISEMENT).contains(&version) {
            // Versions above our range but below MAX_ADVERTISEMENT: clamp to 32
            assert!(result.is_ok(), "Protocol {version} should be clamped to 32");
            assert_eq!(
                result.unwrap().as_u8(),
                32,
                "Protocol {version} should be clamped to 32"
            );
        } else {
            // Versions 0-27 (too old) or > 40 (way too new): reject
            assert!(result.is_err(), "Protocol {version} should be rejected");
        }
    }
}

#[test]
fn test_protocol_matrix_empty_advertisement() {
    // Empty advertisement should fail
    let client_advertises: [TestVersion; 0] = [];
    let result = select_highest_mutual(client_advertises);
    assert!(
        result.is_err(),
        "Empty version advertisement should be rejected"
    );
}

#[test]
fn test_protocol_matrix_version_ordering() {
    // Verify that version selection prefers higher versions
    let test_cases = [
        (vec![28u32, 29, 30], 30u8),
        (vec![30, 31, 32], 32),
        (vec![28, 32], 32),
        (vec![31], 31),
    ];

    for (advertised, expected) in test_cases {
        let client_advertises: Vec<TestVersion> =
            advertised.iter().map(|&v| TestVersion(v)).collect();
        let result = select_highest_mutual(client_advertises);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            expected,
            "Failed for advertised versions {advertised:?}"
        );
    }
}

#[test]
fn test_protocol_matrix_duplicate_versions() {
    // Client advertises duplicate versions (shouldn't happen, but should be handled)
    let client_advertises = [TestVersion(30), TestVersion(30), TestVersion(30)];
    let result = select_highest_mutual(client_advertises);
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_u8(), 30);
}

#[test]
fn test_protocol_matrix_negotiation_stability() {
    // Verify that negotiation is deterministic and stable
    let client_advertises = [TestVersion(32), TestVersion(31), TestVersion(30)];

    for _ in 0..100 {
        let result = select_highest_mutual(client_advertises);
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap().as_u8(),
            32,
            "Negotiation should be deterministic"
        );
    }
}

#[test]
fn test_protocol_matrix_all_versions_roundtrip() {
    // Verify that all supported versions can be negotiated and their properties accessed
    for &version in &SUPPORTED_VERSIONS {
        let client_advertises = [TestVersion(u32::from(version))];
        let result = select_highest_mutual(client_advertises);
        assert!(result.is_ok());

        let negotiated = result.unwrap();
        assert_eq!(negotiated.as_u8(), version);

        // Verify the protocol version object is usable
        let _uses_binary = negotiated.uses_binary_negotiation();
        let _uses_ascii = negotiated.uses_legacy_ascii_negotiation();

        // Verify inverse relationship
        assert_ne!(
            negotiated.uses_binary_negotiation(),
            negotiated.uses_legacy_ascii_negotiation(),
            "Protocol should use exactly one negotiation style"
        );
    }
}
