use proptest::prelude::*;

use super::common::{collect_advertised, reference_negotiation};
use super::select_highest_mutual;

proptest! {
    #[test]
    fn select_highest_mutual_matches_reference(peer_versions in proptest::collection::vec(0u8..=255, 0..=16)) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions.iter().copied());
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_wider_unsigned(
        peer_versions in proptest::collection::vec(any::<u16>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.clone());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_widest_unsigned(
        peer_versions in proptest::collection::vec(any::<u128>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_native_usize(
        peer_versions in proptest::collection::vec(any::<usize>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_signed(
        peer_versions in proptest::collection::vec(any::<i16>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.clone());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_widest_signed(
        peer_versions in proptest::collection::vec(any::<i128>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }

    #[test]
    fn select_highest_mutual_matches_reference_for_native_isize(
        peer_versions in proptest::collection::vec(any::<isize>(), 0..=16)
    ) {
        let advertised = collect_advertised(peer_versions.iter().copied());
        let expected = reference_negotiation(&advertised);
        let actual = select_highest_mutual(peer_versions);
        prop_assert_eq!(actual, expected);
    }
}
