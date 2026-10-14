//! Property tests proving the receive-side reorder buffers reassemble a
//! parallel result stream into the exact serial reference order, byte for
//! byte, no matter how adversarially the results arrive.
//!
//! # What the property asserts
//!
//! The concurrent delta pipeline dispatches per-file work to a rayon worker
//! pool, so results complete in arbitrary order. The reorder layer restores
//! upstream rsync's sequential `recv_files()` ordering before checksum
//! verification and temp-file commit. The contract is exact:
//!
//! > For any arrival permutation of sequences `0..N` and any ring capacity,
//! > the drained output is `0, 1, 2, ..., N-1` - strictly in order, with no
//! > gap-skip, no drop, and no duplicate - and every payload survives
//! > unchanged.
//!
//! This is the serial reference: precisely what a single-threaded processor
//! that consumed the same files in file-list order would emit.
//!
//! # Why this is more than the existing unit tests
//!
//! Prior work (#4552 / #4556) added a `force_insert` counter plus drop /
//! duplicate / monotonicity unit tests, and the reorder module has fixed-seed
//! stress tests. Those exercise `insert` / `drain_ready` directly with a
//! capacity wide enough that `force_insert` never fires. They do not prove the
//! end-to-end reassembled sequence stays correct once the capacity-stall
//! fallback (`force_insert`) engages under adversarial order. These tests
//! drive the exact control flow of the production consumer loop
//! (`consumer/loops.rs`) across many generated permutations and tiny
//! capacities so the fallback is actually taken.
//!
//! # Coverage
//!
//! - `bare_drain_matches_serial_reference` drives [`ReorderBuffer`] through the
//!   `run_bare_loop` flow, including the `force_insert`-under-stall deadlock
//!   breaker.
//! - A deterministic anchor test asserts the stress path is non-vacuous
//!   (`force_insert` genuinely fires) so the property cannot pass trivially.

use std::sync::atomic::Ordering;

use engine::concurrent_delta::ReorderBuffer;
use proptest::prelude::*;

/// A reorder item carrying both its sequence identity and a derived tag.
///
/// The tag is a deterministic function of the sequence, distinct from the
/// sequence itself, so a correct reassembly must place `seq == i` at output
/// position `i` *and* deliver the matching `tag`. This catches three failure
/// classes at once: reordering (`seq != i`), drop / duplicate (length or `seq`
/// mismatch), and payload corruption inside the ring (`tag` mismatch).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Item {
    seq: u64,
    tag: u64,
}

impl Item {
    /// Builds the item for a given sequence with its expected tag.
    fn new(seq: u64) -> Self {
        Self {
            seq,
            tag: Self::tag_for(seq),
        }
    }

    /// Deterministic tag derived from the sequence. Any bijection distinct from
    /// the identity works; this mixes the bits so a swapped payload is visible.
    fn tag_for(seq: u64) -> u64 {
        seq.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1)
    }
}

/// The serial reference for `n` items: `0, 1, ..., n-1`, each with its tag.
///
/// This is the exact sequence a single-threaded, in-order processor would
/// emit. Every reassembly under test must reproduce it byte for byte.
fn serial_reference(n: usize) -> Vec<Item> {
    (0..n as u64).map(Item::new).collect()
}

/// Drives [`ReorderBuffer`] through the production `run_bare_loop` control flow
/// and returns the drained output plus the number of `force_insert`
/// invocations.
///
/// Mirrors `crates/engine/src/concurrent_delta/consumer/loops.rs::run_bare_loop`:
/// try `insert`; on capacity failure drain ready items to free a slot, and if
/// nothing was ready (the head is missing and the ring is full) break the
/// deadlock with `force_insert`; then drain the contiguous run. The output
/// channel is modelled by a `Vec` sink.
fn drive_bare_loop(capacity: usize, arrival: &[u64]) -> (Vec<Item>, u64) {
    let mut reorder: ReorderBuffer<Item> = ReorderBuffer::new(capacity);
    let force_counter = reorder.force_insert_counter();
    let mut out = Vec::with_capacity(arrival.len());

    for &seq in arrival {
        let item = Item::new(seq);
        while reorder.insert(seq, item).is_err() {
            let mut drained_any = false;
            while let Some(ready) = reorder.next_in_order() {
                out.push(ready);
                drained_any = true;
            }
            if !drained_any {
                reorder.force_insert(seq, item);
                break;
            }
        }
        while let Some(ready) = reorder.next_in_order() {
            out.push(ready);
        }
    }

    while let Some(ready) = reorder.next_in_order() {
        out.push(ready);
    }

    let forced = force_counter.load(Ordering::Relaxed);
    (out, forced)
}

/// Strategy yielding `(capacity, arrival)` where `arrival` is a permutation of
/// `0..n` for some `n` in `1..=max_n` and `capacity` is in `1..=n`.
///
/// The permutation is produced by sorting `0..n` by an independent vector of
/// random keys, giving proptest a fully shrinkable adversarial arrival order.
/// A capacity below `n` guarantees the fallback paths are reachable.
fn capacity_and_arrival(max_n: usize) -> impl Strategy<Value = (usize, Vec<u64>)> {
    (1usize..=max_n).prop_flat_map(move |n| {
        (1usize..=n, prop::collection::vec(any::<u64>(), n)).prop_map(move |(capacity, keys)| {
            let mut perm: Vec<u64> = (0..n as u64).collect();
            perm.sort_by_key(|&seq| keys[seq as usize]);
            (capacity, perm)
        })
    })
}

proptest! {
    /// The bare ring buffer, driven through the real consumer loop including
    /// the `force_insert` deadlock breaker, always reassembles the serial
    /// reference regardless of arrival order or capacity.
    #[test]
    fn bare_drain_matches_serial_reference((capacity, arrival) in capacity_and_arrival(128)) {
        let n = arrival.len();
        let (out, _forced) = drive_bare_loop(capacity, &arrival);
        prop_assert_eq!(out, serial_reference(n));
    }

}

/// The bare-loop `force_insert` fallback genuinely fires under a reversed
/// arrival with a capacity far below the gap, and ordering still holds. Guards
/// the property test above from becoming vacuous.
#[test]
fn bare_force_insert_fires_and_preserves_order() {
    const N: usize = 64;
    let arrival: Vec<u64> = (0..N as u64).rev().collect();
    let (out, forced) = drive_bare_loop(2, &arrival);
    assert!(
        forced > 0,
        "reversed arrival with capacity 2 must exercise force_insert"
    );
    assert_eq!(out, serial_reference(N));
}
