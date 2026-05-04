use super::super::*;
use super::super::{chunked_sequences, random_data_and_window, roll_many_sequences};

use std::collections::VecDeque;
use std::io::IoSlice;

proptest! {
    #[test]
    fn rolling_update_matches_single_pass(chunks in chunked_sequences()) {
        let mut incremental = RollingChecksum::new();
        let mut concatenated = Vec::new();

        for chunk in &chunks {
            incremental.update(chunk);
            concatenated.extend_from_slice(chunk);
        }

        let mut single_pass = RollingChecksum::new();
        single_pass.update(&concatenated);

        proptest::prop_assert_eq!(incremental.digest(), single_pass.digest());
        proptest::prop_assert_eq!(incremental.value(), single_pass.value());
    }

    #[test]
    fn rolling_matches_reference_for_random_windows((data, window) in random_data_and_window()) {
        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window]);

        let mut reference = RollingChecksum::new();
        reference.update(&data[..window]);

        proptest::prop_assert_eq!(rolling.digest(), reference.digest());
        proptest::prop_assert_eq!(rolling.value(), reference.value());

        if data.len() > window {
            for start in 1..=data.len() - window {
                let outgoing = data[start - 1];
                let incoming = data[start + window - 1];
                rolling
                    .roll(outgoing, incoming)
                    .expect("rolling update must succeed");

                let mut recomputed = RollingChecksum::new();
                recomputed.update(&data[start..start + window]);

                proptest::prop_assert_eq!(rolling.digest(), recomputed.digest());
                proptest::prop_assert_eq!(rolling.value(), recomputed.value());
            }
        }
    }

    #[test]
    fn vectored_update_matches_chunked_input(chunks in chunked_sequences()) {
        let mut sequential = RollingChecksum::new();
        for chunk in &chunks {
            sequential.update(chunk);
        }

        let slices: Vec<IoSlice<'_>> =
            chunks.iter().map(|chunk| IoSlice::new(chunk.as_slice())).collect();

        let mut vectored = RollingChecksum::new();
        vectored.update_vectored(&slices);

        proptest::prop_assert_eq!(vectored.digest(), sequential.digest());
        proptest::prop_assert_eq!(vectored.value(), sequential.value());
    }

    #[test]
    fn roll_many_matches_single_rolls_for_random_sequences(
        (seed, pairs) in roll_many_sequences(),
    ) {
        let mut optimized = RollingChecksum::new();
        optimized.update(&seed);

        let mut reference = optimized.clone();

        let (outgoing, incoming): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
        optimized
            .roll_many(&outgoing, &incoming)
            .expect("multi-byte roll succeeds");

        for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
            reference
                .roll(out, inn)
                .expect("single-byte roll succeeds");
        }

        proptest::prop_assert_eq!(optimized.digest(), reference.digest());
        proptest::prop_assert_eq!(optimized.value(), reference.value());
    }

    #[test]
    fn from_digest_round_trips(data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..=256)) {
        let mut checksum = RollingChecksum::new();
        checksum.update(&data);

        let digest = checksum.digest();
        let restored = RollingChecksum::from_digest(digest);

        proptest::prop_assert_eq!(restored.digest(), digest);
        proptest::prop_assert_eq!(restored.value(), checksum.value());
        proptest::prop_assert_eq!(restored.len(), checksum.len());
    }
}

#[test]
fn roll_many_matches_single_rolls_for_long_sequences() {
    let seed: Vec<u8> = (0..128)
        .map(|value| {
            let byte = value as u8;
            byte.wrapping_mul(13).wrapping_add(5)
        })
        .collect();

    let mut batched = RollingChecksum::new();
    batched.update(&seed);

    let mut reference = batched.clone();

    let mut window = VecDeque::from(seed.clone());
    let mut outgoing = Vec::with_capacity(4096);
    let mut incoming = Vec::with_capacity(4096);

    for step in 0..4096 {
        let leaving = window
            .pop_front()
            .expect("rolling checksum window must contain data");
        let step_byte = step as u8;
        let entering = step_byte
            .wrapping_mul(17)
            .wrapping_add(23)
            .wrapping_add(step_byte >> 3);

        outgoing.push(leaving);
        incoming.push(entering);
        window.push_back(entering);
    }

    batched
        .roll_many(&outgoing, &incoming)
        .expect("batched roll succeeds");

    for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
        reference.roll(out, inn).expect("sequential roll succeeds");
    }

    assert_eq!(batched.digest(), reference.digest());
    assert_eq!(batched.value(), reference.value());
}
