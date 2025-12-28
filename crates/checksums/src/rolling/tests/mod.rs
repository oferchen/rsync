use super::*;

use proptest::prelude::*;

pub(super) fn reference_digest(data: &[u8]) -> RollingDigest {
    let mut s1: u64 = 0;
    let mut s2: u64 = 0;

    for &byte in data {
        s1 += u64::from(byte);
        s2 += s1;
    }

    RollingDigest::new((s1 & 0xffff) as u16, (s2 & 0xffff) as u16, data.len())
}

pub(super) fn random_data_and_window() -> impl Strategy<Value = (Vec<u8>, usize)> {
    prop::collection::vec(any::<u8>(), 1..=256).prop_flat_map(|data| {
        let len = data.len();
        let window_range = 1..=len;
        (Just(data), window_range).prop_map(|(data, window)| (data, window))
    })
}

pub(super) fn chunked_sequences() -> impl Strategy<Value = Vec<Vec<u8>>> {
    prop::collection::vec(prop::collection::vec(any::<u8>(), 0..=64), 1..=8)
}

pub(super) fn roll_many_sequences() -> impl Strategy<Value = (Vec<u8>, Vec<(u8, u8)>)> {
    prop::collection::vec(any::<u8>(), 1..=64).prop_flat_map(|seed| {
        let seed_clone = seed;
        prop::collection::vec((any::<u8>(), any::<u8>()), 0..=32)
            .prop_map(move |pairs| (seed_clone.clone(), pairs))
    })
}

mod checksum;
mod digest;
