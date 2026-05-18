//! Tests for the shared permission-bit conversion helpers.

use crate::acl_windows::common::{access_mask_to_rsync_perms, rsync_perms_to_access_mask};

#[test]
fn perms_round_trip_through_access_mask() {
    for perms in 0u8..=0b111 {
        let mask = rsync_perms_to_access_mask(perms);
        let back = access_mask_to_rsync_perms(mask);
        assert_eq!(back, perms, "round-trip failed for {perms:03b}");
    }
}
