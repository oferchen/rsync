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
//! fallback (`force_insert`) or the disk spill path engages under adversarial
//! order. These tests drive the exact control flow of the production consumer
//! loops (`consumer/loops.rs`) across many generated permutations and tiny
//! capacities so both fallbacks are actually taken.
//!
//! # Coverage
//!
//! - `bare_drain_matches_serial_reference` drives [`ReorderBuffer`] through the
//!   `run_bare_loop` flow, including the `force_insert`-under-stall deadlock
//!   breaker.
//! - `spill_disk_reassembly_matches_serial_reference` drives
//!   [`SpillableReorderBuffer`] with a byte threshold small enough to force
//!   real disk spill + reload, mirroring the `run_spillable_loop` flow. This is
//!   the disk-reassembly determinism proof.
//! - Deterministic anchor tests assert the stress paths are non-vacuous
//!   (`force_insert` and disk spill genuinely fire) so the properties cannot
//!   pass trivially.
//!
//! The ENOSPC loud-failure contract (a spill hitting a full disk surfaces a
//! typed error and never silently drops or reorders) is proven at the unit
//! level in `spill/buffer/tests/enospc_degradation.rs` and is not duplicated
//! here.

use std::io::{self, Read, Write};
use std::sync::atomic::Ordering;

use engine::concurrent_delta::{ReorderBuffer, SpillCodec, SpillError, SpillableReorderBuffer};
use proptest::prelude::*;

/// A reorder item carrying both its sequence identity and a derived tag.
///
/// The tag is a deterministic function of the sequence, distinct from the
/// sequence itself, so a correct reassembly must place `seq == i` at output
/// position `i` *and* deliver the matching `tag`. This catches three failure
/// classes at once: reordering (`seq != i`), drop / duplicate (length or `seq`
/// mismatch), and payload corruption across the spill round-trip (`tag`
/// mismatch).
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

impl SpillCodec for Item {
    fn encode(&self, writer: &mut dyn Write) -> io::Result<()> {
        writer.write_all(&self.seq.to_le_bytes())?;
        writer.write_all(&self.tag.to_le_bytes())
    }

    fn decode(reader: &mut dyn Read) -> io::Result<Self> {
        let mut buf = [0u8; 8];
        reader.read_exact(&mut buf)?;
        let seq = u64::from_le_bytes(buf);
        reader.read_exact(&mut buf)?;
        let tag = u64::from_le_bytes(buf);
        Ok(Self { seq, tag })
    }

    fn estimated_size(&self) -> usize {
        16
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

/// Drives [`SpillableReorderBuffer`] through the production `run_spillable_loop`
/// control flow and returns the drained output.
///
/// Mirrors `crates/engine/src/concurrent_delta/consumer/loops.rs::run_spillable_loop`:
/// a small `byte_threshold` forces the buffer to serialise higher-sequence
/// items to a tempfile and reload them on drain. Any non-capacity
/// [`SpillError`] (or a failed `force_insert`) is surfaced as `Err` - the
/// loud-failure contract - rather than silently dropped.
fn drive_spillable_loop(
    ring_capacity: usize,
    byte_threshold: usize,
    arrival: &[u64],
) -> Result<Vec<Item>, String> {
    let mut reorder: SpillableReorderBuffer<Item> =
        SpillableReorderBuffer::new(ring_capacity, byte_threshold);
    let mut out = Vec::with_capacity(arrival.len());

    for &seq in arrival {
        let item = Item::new(seq);
        loop {
            match reorder.insert(seq, item) {
                Ok(()) => break,
                Err(SpillError::Capacity(_)) => {
                    let mut drained_any = false;
                    for ready in reorder.drain_ready().map_err(|e| e.to_string())? {
                        out.push(ready);
                        drained_any = true;
                    }
                    if !drained_any {
                        reorder.force_insert(seq, item).map_err(|e| e.to_string())?;
                        break;
                    }
                }
                Err(other) => return Err(other.to_string()),
            }
        }
        for ready in reorder.drain_ready().map_err(|e| e.to_string())? {
            out.push(ready);
        }
    }

    loop {
        let items = reorder.drain_ready().map_err(|e| e.to_string())?;
        if items.is_empty() {
            break;
        }
        out.extend(items);
    }

    Ok(out)
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

/// Two items' worth of bytes: the byte threshold that forces the spillable
/// buffer to serialise the third and later held-back items to disk.
const SPILL_THRESHOLD: usize = 2 * 16;

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

    /// The spillable buffer with a byte threshold small enough to force disk
    /// spill + reload reassembles the serial reference under adversarial
    /// arrival. This proves disk reassembly is deterministic, not just the
    /// in-memory ring.
    #[test]
    fn spill_disk_reassembly_matches_serial_reference(
        (ring_capacity, arrival) in capacity_and_arrival(48),
    ) {
        let n = arrival.len();
        let out = drive_spillable_loop(ring_capacity, SPILL_THRESHOLD, &arrival)
            .map_err(|e| TestCaseError::fail(format!("spill loop failed: {e}")))?;
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

/// The spillable-loop disk path genuinely engages (items are written to the
/// tempfile) under a reversed arrival with a tiny byte threshold, and the
/// reloaded stream still matches the serial reference. Guards the disk property
/// test from becoming vacuous.
#[test]
fn spill_disk_path_engages_and_preserves_order() {
    const N: usize = 64;
    let arrival: Vec<u64> = (0..N as u64).rev().collect();
    let mut reorder: SpillableReorderBuffer<Item> = SpillableReorderBuffer::new(N, SPILL_THRESHOLD);
    for &seq in &arrival {
        reorder.insert(seq, Item::new(seq)).unwrap();
    }
    assert!(
        reorder.spill_stats().spill_events > 0,
        "reversed arrival under a tiny threshold must spill to disk"
    );

    let out =
        drive_spillable_loop(N, SPILL_THRESHOLD, &arrival).expect("spill reassembly must succeed");
    assert_eq!(out, serial_reference(N));
}
